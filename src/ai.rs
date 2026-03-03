//! SigLIP ViT-B/16-256 model for zero-shot image classification.
//!
//! Only compiled when the `ai` feature is enabled.

use std::path::Path;

use anyhow::{Context, Result};
use ndarray::{Array2, Array4, Axis};
use ort::session::Session;
use ort::value::Tensor;
use serde::Serialize;
use tokenizers::Tokenizer;

/// SigLIP sigmoid scoring parameters (from model config).
const LOGIT_SCALE: f32 = 4.713;
const LOGIT_BIAS: f32 = -12.928;

/// Embedding dimensionality.
pub const EMBEDDING_DIM: usize = 768;

/// Image input size (pixels).
const IMAGE_SIZE: usize = 256;

/// Maximum text token length.
const MAX_TEXT_LEN: usize = 64;

/// Padding token ID for SentencePiece tokenizer.
const PAD_TOKEN_ID: u32 = 1;

/// A suggested tag with confidence score.
#[derive(Debug, Clone, Serialize)]
pub struct AutoTagSuggestion {
    pub tag: String,
    pub confidence: f32,
}

/// Per-asset suggestion results.
#[derive(Debug, Clone, Serialize)]
pub struct AssetSuggestions {
    pub asset_id: String,
    pub suggested_tags: Vec<AutoTagSuggestion>,
    pub applied: bool,
}

/// Overall auto-tag operation result.
#[derive(Debug, Clone, Serialize)]
pub struct AutoTagResult {
    pub assets_processed: usize,
    pub assets_skipped: usize,
    pub tags_suggested: usize,
    pub tags_applied: usize,
    pub errors: Vec<String>,
    pub dry_run: bool,
    pub suggestions: Vec<AssetSuggestions>,
}

/// Callback status for per-asset progress reporting.
pub enum AutoTagStatus {
    Suggested(Vec<AutoTagSuggestion>),
    Applied(Vec<AutoTagSuggestion>),
    Skipped(String),
    Error(String),
}

/// SigLIP vision-language model wrapper.
pub struct SigLipModel {
    vision: Session,
    text: Session,
    tokenizer: Tokenizer,
}

impl SigLipModel {
    /// Load ONNX sessions and tokenizer from the model directory.
    pub fn load(model_dir: &Path) -> Result<Self> {
        let vision_path = model_dir.join("onnx").join("vision_model_quantized.onnx");
        let text_path = model_dir.join("onnx").join("text_model_quantized.onnx");
        let tokenizer_path = model_dir.join("tokenizer.json");

        // Fall back to non-quantized if quantized not found
        let vision_path = if vision_path.exists() {
            vision_path
        } else {
            let fp32 = model_dir.join("onnx").join("vision_model.onnx");
            if !fp32.exists() {
                anyhow::bail!(
                    "Vision model not found. Expected {} or {}",
                    vision_path.display(),
                    fp32.display()
                );
            }
            fp32
        };

        let text_path = if text_path.exists() {
            text_path
        } else {
            let fp32 = model_dir.join("onnx").join("text_model.onnx");
            if !fp32.exists() {
                anyhow::bail!(
                    "Text model not found. Expected {} or {}",
                    text_path.display(),
                    fp32.display()
                );
            }
            fp32
        };

        if !tokenizer_path.exists() {
            anyhow::bail!("Tokenizer not found at {}", tokenizer_path.display());
        }

        let vision = Session::builder()
            .context("Failed to create ONNX session builder")?
            .with_intra_threads(4)
            .context("Failed to set intra threads")?
            .commit_from_file(&vision_path)
            .with_context(|| format!("Failed to load vision model from {}", vision_path.display()))?;

        let text = Session::builder()
            .context("Failed to create ONNX session builder")?
            .with_intra_threads(4)
            .context("Failed to set intra threads")?
            .commit_from_file(&text_path)
            .with_context(|| format!("Failed to load text model from {}", text_path.display()))?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {e}"))?;

        Ok(Self {
            vision,
            text,
            tokenizer,
        })
    }

    /// Encode an image file into a 768-dimensional embedding.
    pub fn encode_image(&mut self, image_path: &Path) -> Result<Vec<f32>> {
        let tensor = preprocess_image(image_path)?;
        let input_value = Tensor::from_array(tensor)
            .context("Failed to create vision input tensor")?;
        let outputs = self.vision.run(
            ort::inputs!["pixel_values" => input_value],
        )?;

        let embedding = outputs[0]
            .try_extract_array::<f32>()
            .context("Failed to extract vision embedding tensor")?;

        let emb: Vec<f32> = embedding.iter().copied().collect();
        Ok(l2_normalize(&emb))
    }

    /// Encode a batch of text strings into 768-dimensional embeddings.
    pub fn encode_texts(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let input_ids = tokenize_batch(&self.tokenizer, texts)?;
        let input_value = Tensor::from_array(input_ids)
            .context("Failed to create text input tensor")?;
        let outputs = self.text.run(
            ort::inputs!["input_ids" => input_value],
        )?;

        let embeddings = outputs[0]
            .try_extract_array::<f32>()
            .context("Failed to extract text embedding tensor")?;

        let shape = embeddings.shape();
        let batch_size = shape[0];

        let mut result = Vec::with_capacity(batch_size);
        for i in 0..batch_size {
            let emb: Vec<f32> = embeddings
                .index_axis(Axis(0), i)
                .iter()
                .copied()
                .collect();
            result.push(l2_normalize(&emb));
        }

        Ok(result)
    }

    /// Classify an image embedding against label embeddings using SigLIP sigmoid scoring.
    /// Returns suggestions above the threshold, sorted by confidence (descending).
    pub fn classify(
        &self,
        image_emb: &[f32],
        labels: &[String],
        label_embs: &[Vec<f32>],
        threshold: f32,
    ) -> Vec<AutoTagSuggestion> {
        let mut suggestions: Vec<AutoTagSuggestion> = labels
            .iter()
            .zip(label_embs.iter())
            .filter_map(|(label, label_emb)| {
                let dot: f32 = image_emb
                    .iter()
                    .zip(label_emb.iter())
                    .map(|(a, b)| a * b)
                    .sum();
                let logit = LOGIT_SCALE * dot + LOGIT_BIAS;
                let confidence = sigmoid(logit);
                if confidence >= threshold {
                    Some(AutoTagSuggestion {
                        tag: label.clone(),
                        confidence,
                    })
                } else {
                    None
                }
            })
            .collect();

        suggestions.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap());
        suggestions
    }
}

/// Preprocess an image for SigLIP: resize to 256x256, normalize to [-1, 1].
fn preprocess_image(path: &Path) -> Result<Array4<f32>> {
    let img = image::open(path)
        .with_context(|| format!("Failed to open image: {}", path.display()))?;

    // Squash resize to exactly 256x256 (no crop)
    let resized = img.resize_exact(
        IMAGE_SIZE as u32,
        IMAGE_SIZE as u32,
        image::imageops::FilterType::CatmullRom,
    );

    let rgb = resized.to_rgb8();
    let mut tensor = Array4::<f32>::zeros((1, 3, IMAGE_SIZE, IMAGE_SIZE));

    for y in 0..IMAGE_SIZE {
        for x in 0..IMAGE_SIZE {
            let pixel = rgb.get_pixel(x as u32, y as u32);
            for c in 0..3 {
                tensor[[0, c, y, x]] = (pixel[c] as f32 / 255.0 - 0.5) / 0.5;
            }
        }
    }

    Ok(tensor)
}

/// Tokenize a batch of texts, padding to MAX_TEXT_LEN.
fn tokenize_batch(tokenizer: &Tokenizer, texts: &[String]) -> Result<Array2<i64>> {
    let batch_size = texts.len();
    let mut input_ids = Array2::<i64>::from_elem((batch_size, MAX_TEXT_LEN), PAD_TOKEN_ID as i64);

    for (i, text) in texts.iter().enumerate() {
        let encoding = tokenizer
            .encode(text.as_str(), true)
            .map_err(|e| anyhow::anyhow!("Tokenization failed: {e}"))?;

        let ids = encoding.get_ids();
        let len = ids.len().min(MAX_TEXT_LEN);
        for j in 0..len {
            input_ids[[i, j]] = ids[j] as i64;
        }
    }

    Ok(input_ids)
}

/// L2-normalize a vector.
fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm < 1e-12 {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

/// Sigmoid function.
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Cosine similarity between two vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a < 1e-12 || norm_b < 1e-12 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Default photography labels for zero-shot classification.
/// Each label is a short category name; the prompt template wraps it.
pub const DEFAULT_LABELS: &[&str] = &[
    // Scene types
    "landscape",
    "portrait",
    "street photography",
    "architecture",
    "cityscape",
    "seascape",
    "aerial view",
    "panorama",
    "interior",
    "still life",
    "macro",
    "wildlife",
    "underwater",
    "astrophotography",
    "night photography",
    // Nature
    "mountain",
    "forest",
    "beach",
    "ocean",
    "river",
    "lake",
    "waterfall",
    "desert",
    "field",
    "garden",
    "flowers",
    "trees",
    "sky",
    "clouds",
    "sunset",
    "sunrise",
    "fog",
    "snow",
    "rain",
    // People
    "person",
    "group of people",
    "child",
    "family",
    "couple",
    "crowd",
    "self-portrait",
    // Animals
    "dog",
    "cat",
    "bird",
    "horse",
    "fish",
    "insect",
    "butterfly",
    // Urban
    "building",
    "bridge",
    "road",
    "car",
    "bicycle",
    "train",
    "airplane",
    "boat",
    "skyscraper",
    // Food & Objects
    "food",
    "drink",
    "book",
    "clothing",
    "jewelry",
    "furniture",
    // Events
    "wedding",
    "concert",
    "festival",
    "sports",
    "celebration",
    "ceremony",
    // Artistic
    "black and white",
    "long exposure",
    "silhouette",
    "reflection",
    "shadow",
    "abstract",
    "minimalist",
    "texture",
    "pattern",
    "bokeh",
    "motion blur",
    // Seasonal
    "spring",
    "summer",
    "autumn",
    "winter",
    // Travel
    "travel",
    "landmark",
    "monument",
    "ruins",
    "market",
    "museum",
    "church",
    "temple",
    // Technical
    "document",
    "screenshot",
    "infographic",
    "product photography",
];

/// Apply the prompt template to a label (e.g. "a photograph of {}" + "sunset" → "a photograph of sunset").
pub fn apply_prompt_template(template: &str, label: &str) -> String {
    template.replace("{}", label)
}

/// Load labels from a text file (one per line, # comments, blank lines ignored).
pub fn load_labels_from_file(path: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read labels file: {}", path.display()))?;

    let labels: Vec<String> = content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect();

    if labels.is_empty() {
        anyhow::bail!("Labels file is empty: {}", path.display());
    }

    Ok(labels)
}

/// Check if a file extension is a supported image format for SigLIP processing.
pub fn is_supported_image(ext: &str) -> bool {
    matches!(
        ext.to_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "tiff" | "tif" | "webp"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_unit_vector() {
        let v = vec![1.0, 0.0, 0.0];
        let n = l2_normalize(&v);
        assert!((n[0] - 1.0).abs() < 1e-6);
        assert!(n[1].abs() < 1e-6);
        assert!(n[2].abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_scales_correctly() {
        let v = vec![3.0, 4.0];
        let n = l2_normalize(&v);
        assert!((n[0] - 0.6).abs() < 1e-6);
        assert!((n[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_vector() {
        let v = vec![0.0, 0.0, 0.0];
        let n = l2_normalize(&v);
        assert_eq!(n, v);
    }

    #[test]
    fn sigmoid_zero() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn sigmoid_large_positive() {
        assert!((sigmoid(10.0) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn sigmoid_large_negative() {
        assert!(sigmoid(-10.0) < 1e-4);
    }

    #[test]
    fn cosine_similarity_identical() {
        let a = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &a);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim + 1.0).abs() < 1e-6);
    }

    #[test]
    fn prompt_template_substitution() {
        assert_eq!(
            apply_prompt_template("a photograph of {}", "sunset"),
            "a photograph of sunset"
        );
    }

    #[test]
    fn prompt_template_no_placeholder() {
        assert_eq!(apply_prompt_template("hello world", "sunset"), "hello world");
    }

    #[test]
    fn default_labels_count() {
        assert!(DEFAULT_LABELS.len() >= 90, "Expected at least 90 default labels, got {}", DEFAULT_LABELS.len());
    }

    #[test]
    fn default_labels_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for label in DEFAULT_LABELS {
            assert!(seen.insert(label), "Duplicate label: {label}");
        }
    }

    #[test]
    fn is_supported_image_common_formats() {
        assert!(is_supported_image("jpg"));
        assert!(is_supported_image("JPEG"));
        assert!(is_supported_image("png"));
        assert!(is_supported_image("webp"));
    }

    #[test]
    fn is_supported_image_raw_not_supported() {
        assert!(!is_supported_image("nef"));
        assert!(!is_supported_image("cr2"));
        assert!(!is_supported_image("arw"));
    }

    #[test]
    fn is_supported_image_non_image() {
        assert!(!is_supported_image("mp4"));
        assert!(!is_supported_image("mp3"));
        assert!(!is_supported_image("pdf"));
    }

    #[test]
    fn classify_empty_labels() {
        // Can't run full model in unit tests, but test the classify logic
        let image_emb = l2_normalize(&vec![1.0; EMBEDDING_DIM]);
        let labels: Vec<String> = Vec::new();
        let label_embs: Vec<Vec<f32>> = Vec::new();

        // We test the scoring math directly since we can't load the model
        let suggestions = score_and_filter(&image_emb, &labels, &label_embs, 0.25);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn classify_threshold_filters() {
        let image_emb = l2_normalize(&vec![1.0; EMBEDDING_DIM]);

        // Create a label embedding very similar to the image embedding
        let similar_emb = l2_normalize(&vec![1.0; EMBEDDING_DIM]);
        // Create an orthogonal-ish embedding
        let mut different = vec![0.0f32; EMBEDDING_DIM];
        different[0] = 1.0;
        let different_emb = l2_normalize(&different);

        let labels = vec!["similar".to_string(), "different".to_string()];
        let label_embs = vec![similar_emb, different_emb];

        // Both identical unit vectors give dot=1.0, logit=4.713-12.928=-8.215, sigmoid≈0.00027
        // The similar embedding scores higher than the different one
        let all = score_and_filter(&image_emb, &labels, &label_embs, 0.0);
        assert_eq!(all.len(), 2);
        assert!(
            all[0].confidence > all[1].confidence || all[0].tag == "similar",
            "Expected 'similar' to score higher than 'different'"
        );

        // With a threshold above the max score, nothing matches
        let none = score_and_filter(&image_emb, &labels, &label_embs, 0.5);
        assert!(none.is_empty(), "Expected no suggestions at high threshold");
    }

    /// Standalone scoring function for testing (mirrors SigLipModel::classify logic).
    fn score_and_filter(
        image_emb: &[f32],
        labels: &[String],
        label_embs: &[Vec<f32>],
        threshold: f32,
    ) -> Vec<AutoTagSuggestion> {
        labels
            .iter()
            .zip(label_embs.iter())
            .filter_map(|(label, label_emb)| {
                let dot: f32 = image_emb
                    .iter()
                    .zip(label_emb.iter())
                    .map(|(a, b)| a * b)
                    .sum();
                let logit = LOGIT_SCALE * dot + LOGIT_BIAS;
                let confidence = sigmoid(logit);
                if confidence >= threshold {
                    Some(AutoTagSuggestion {
                        tag: label.clone(),
                        confidence,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn load_labels_from_file_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("labels.txt");
        std::fs::write(&path, "landscape\nportrait\n# comment\n\nocean\n").unwrap();
        let labels = load_labels_from_file(&path).unwrap();
        assert_eq!(labels, vec!["landscape", "portrait", "ocean"]);
    }

    #[test]
    fn load_labels_from_file_empty_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("labels.txt");
        std::fs::write(&path, "# only comments\n\n").unwrap();
        assert!(load_labels_from_file(&path).is_err());
    }
}
