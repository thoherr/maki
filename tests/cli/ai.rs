//! AI and Pro commands: auto-tag, auto-stack, faces, describe, vocabulary and embedding export.

use crate::common::*;

#[test]
fn ai_export_vocabulary_honours_json() {
    let out = maki()
        .args(["--json", "ai", "export-vocabulary", "--default"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["source"], "builtin-default");
    assert!(v["output"].is_null());
    assert!(v["yaml"].as_str().unwrap().contains(':'), "yaml document wrapped in JSON");
    assert!(v["bytes"].as_u64().unwrap() > 0);

    // With --output, stdout is a JSON summary and the file holds the YAML.
    let dir = tempdir().unwrap();
    let path = dir.path().join("vocab.yaml");
    let out = maki()
        .args(["--json", "ai", "export-vocabulary", "--default", "--output", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["output"], path.to_str().unwrap());
    assert!(std::fs::read_to_string(&path).unwrap().contains(':'));
}

mod export_ai_data {
    use super::*;

    #[test]
    fn faces_export_empty_catalog() {
        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        maki()
            .current_dir(&root)
            .args(["faces", "export"])
            .assert()
            .success();

        // YAML files should be created even if empty
        assert!(root.join("faces.yaml").exists());
        assert!(root.join("people.yaml").exists());
    }

    #[test]
    fn faces_export_json_output() {
        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        let output = maki()
            .current_dir(&root)
            .args(["faces", "export", "--json"])
            .assert()
            .success();

        let stdout = String::from_utf8_lossy(&output.get_output().stdout);
        let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert!(parsed.get("faces").is_some());
        assert!(parsed.get("people").is_some());
        assert!(parsed.get("arcface_binaries").is_some());
    }

    #[test]
    fn embed_export_empty_catalog() {
        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        maki()
            .current_dir(&root)
            .args(["embed", "--export"])
            .assert()
            .success()
            .stdout(predicate::str::contains("Exported 0 embedding binaries"));
    }

    #[test]
    fn embed_export_json_output() {
        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        let output = maki()
            .current_dir(&root)
            .args(["embed", "--export", "--json"])
            .assert()
            .success();

        let stdout = String::from_utf8_lossy(&output.get_output().stdout);
        let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(parsed["exported"], 0);
        assert!(parsed["models"].is_array());
    }
}

mod auto_tag {
    use super::*;

    /// Check if the SigLIP model is downloaded.
    fn model_available() -> bool {
        let model_dir = dirs_model_dir();
        model_dir.join("onnx").join("vision_model_quantized.onnx").exists()
            && model_dir.join("onnx").join("text_model_quantized.onnx").exists()
            && model_dir.join("tokenizer.json").exists()
    }

    fn dirs_model_dir() -> PathBuf {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap();
        PathBuf::from(home)
            .join(".maki")
            .join("models")
            .join("siglip-vit-b16-256")
    }

    #[test]
    fn auto_tag_list_models_no_model() {
        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        // Use a custom empty model dir so we don't depend on real model
        maki()
            .current_dir(&root)
            .args(["auto-tag", "--list-models"])
            .assert()
            .success();
    }

    #[test]
    fn auto_tag_list_models_json() {
        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        let output = maki()
            .current_dir(&root)
            .args(["auto-tag", "--list-models", "--json"])
            .assert()
            .success();

        let stdout = String::from_utf8_lossy(&output.get_output().stdout);
        let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert!(parsed.get("model_dir").is_some());
        assert!(parsed.get("active_model").is_some());
        let models = parsed.get("models").unwrap().as_array().unwrap();
        assert!(models.len() >= 2, "Expected at least 2 models");
        // Check that each model has expected fields
        for m in models {
            assert!(m.get("id").is_some());
            assert!(m.get("name").is_some());
            assert!(m.get("downloaded").is_some());
            assert!(m.get("embedding_dim").is_some());
        }
    }

    #[test]
    fn auto_tag_model_flag() {
        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        // Use --model with an unknown model
        maki()
            .current_dir(&root)
            .args(["auto-tag", "--model", "nonexistent-model", "--list-models"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("Unknown model"));
    }

    #[test]
    fn auto_tag_no_scope_errors() {
        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        // Running auto-tag without --query/--asset/--volume should fail
        maki()
            .current_dir(&root)
            .args(["auto-tag"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("No scope specified"));
    }

    #[test]
    fn auto_tag_no_model_errors() {
        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        // Override model dir to an empty location
        let config_path = root.join("maki.toml");
        let model_tmp = tempdir().unwrap();
        std::fs::write(
            &config_path,
            format!(
                "[ai]\nmodel_dir = \"{}\"\n",
                model_tmp.path().display().to_string().replace('\\', "/")
            ),
        )
        .unwrap();

        maki()
            .current_dir(&root)
            .args(["auto-tag", "*"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("Model not downloaded"));
    }

    #[test]
    fn auto_tag_remove_model_nonexistent() {
        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        // Override model dir to a temp location
        let config_path = root.join("maki.toml");
        let model_tmp = tempdir().unwrap();
        std::fs::write(
            &config_path,
            format!(
                "[ai]\nmodel_dir = \"{}\"\n",
                model_tmp.path().display().to_string().replace('\\', "/")
            ),
        )
        .unwrap();

        maki()
            .current_dir(&root)
            .args(["auto-tag", "--remove-model"])
            .assert()
            .success();
    }

    // The following tests require the model to be downloaded.
    // They are skipped gracefully if the model is not available.

    #[test]
    fn auto_tag_dry_run() {
        if !model_available() {
            eprintln!("Skipping auto_tag_dry_run: model not downloaded");
            return;
        }

        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        // Create a real JPEG (small 2x2 image)
        let img = image::RgbImage::from_fn(2, 2, |_, _| image::Rgb([128, 64, 200]));
        let img_path = root.join("test.jpg");
        img.save(&img_path).unwrap();

        // Import
        maki()
            .current_dir(&root)
            .args(["import", img_path.to_str().unwrap()])
            .assert()
            .success();

        // Auto-tag dry run
        maki()
            .current_dir(&root)
            .args(["auto-tag", "type:image"])
            .assert()
            .success()
            .stdout(predicate::str::contains("dry run"));
    }

    #[test]
    fn auto_tag_apply() {
        if !model_available() {
            eprintln!("Skipping auto_tag_apply: model not downloaded");
            return;
        }

        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        let img = image::RgbImage::from_fn(2, 2, |_, _| image::Rgb([128, 64, 200]));
        let img_path = root.join("test.jpg");
        img.save(&img_path).unwrap();

        maki()
            .current_dir(&root)
            .args(["import", img_path.to_str().unwrap()])
            .assert()
            .success();

        maki()
            .current_dir(&root)
            .args([
                "auto-tag",
                "type:image",
                "--apply",
                "--threshold",
                "0.01",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("processed"));
    }

    #[test]
    fn auto_tag_json_output() {
        if !model_available() {
            eprintln!("Skipping auto_tag_json_output: model not downloaded");
            return;
        }

        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        let img = image::RgbImage::from_fn(2, 2, |_, _| image::Rgb([128, 64, 200]));
        let img_path = root.join("test.jpg");
        img.save(&img_path).unwrap();

        maki()
            .current_dir(&root)
            .args(["import", img_path.to_str().unwrap()])
            .assert()
            .success();

        let output = maki()
            .current_dir(&root)
            .args(["auto-tag", "type:image", "--json"])
            .assert()
            .success();

        let stdout = String::from_utf8_lossy(&output.get_output().stdout);
        let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert!(parsed.get("assets_processed").is_some());
        assert!(parsed.get("dry_run").is_some());
        assert!(parsed.get("suggestions").is_some());
    }

    #[test]
    fn auto_tag_custom_labels() {
        if !model_available() {
            eprintln!("Skipping auto_tag_custom_labels: model not downloaded");
            return;
        }

        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        let img = image::RgbImage::from_fn(2, 2, |_, _| image::Rgb([128, 64, 200]));
        let img_path = root.join("test.jpg");
        img.save(&img_path).unwrap();

        maki()
            .current_dir(&root)
            .args(["import", img_path.to_str().unwrap()])
            .assert()
            .success();

        // Create custom labels file
        let labels_path = root.join("my_labels.txt");
        std::fs::write(&labels_path, "purple\nblue\nred\n").unwrap();

        maki()
            .current_dir(&root)
            .args([
                "auto-tag",
                "type:image",
                "--labels",
                labels_path.to_str().unwrap(),
            ])
            .assert()
            .success();
    }

    #[test]
    fn auto_tag_threshold_high() {
        if !model_available() {
            eprintln!("Skipping auto_tag_threshold_high: model not downloaded");
            return;
        }

        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        let img = image::RgbImage::from_fn(2, 2, |_, _| image::Rgb([128, 64, 200]));
        let img_path = root.join("test.jpg");
        img.save(&img_path).unwrap();

        maki()
            .current_dir(&root)
            .args(["import", img_path.to_str().unwrap()])
            .assert()
            .success();

        // High threshold = fewer/no suggestions
        let output = maki()
            .current_dir(&root)
            .args([
                "auto-tag",
                "type:image",
                "--threshold",
                "0.99",
                "--json",
            ])
            .assert()
            .success();

        let stdout = String::from_utf8_lossy(&output.get_output().stdout);
        let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(parsed["tags_suggested"].as_u64().unwrap(), 0);
    }

    #[test]
    fn auto_tag_specific_asset() {
        if !model_available() {
            eprintln!("Skipping auto_tag_specific_asset: model not downloaded");
            return;
        }

        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        let img = image::RgbImage::from_fn(2, 2, |_, _| image::Rgb([128, 64, 200]));
        let img_path = root.join("test.jpg");
        img.save(&img_path).unwrap();

        // Import
        maki()
            .current_dir(&root)
            .args(["import", img_path.to_str().unwrap()])
            .assert()
            .success();

        // Get asset ID via search
        let search_output = maki()
            .current_dir(&root)
            .args(["search", "-q", "type:image"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let asset_id = String::from_utf8(search_output).unwrap();
        let asset_id = asset_id.trim();
        let short_id = &asset_id[..8];

        maki()
            .current_dir(&root)
            .args(["auto-tag", "--asset", short_id])
            .assert()
            .success()
            .stdout(predicate::str::contains("processed"));
    }

    #[test]
    fn auto_tag_similar() {
        if !model_available() {
            eprintln!("Skipping auto_tag_similar: model not downloaded");
            return;
        }

        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        // Create two images
        let img1 = image::RgbImage::from_fn(2, 2, |_, _| image::Rgb([128, 64, 200]));
        let img_path1 = root.join("test1.jpg");
        img1.save(&img_path1).unwrap();

        let img2 = image::RgbImage::from_fn(2, 2, |_, _| image::Rgb([200, 100, 50]));
        let img_path2 = root.join("test2.jpg");
        img2.save(&img_path2).unwrap();

        maki()
            .current_dir(&root)
            .args(["import", img_path1.to_str().unwrap(), img_path2.to_str().unwrap()])
            .assert()
            .success();

        // Auto-tag both to generate embeddings
        maki()
            .current_dir(&root)
            .args(["auto-tag", "type:image"])
            .assert()
            .success();

        // Get first asset ID
        let search_output = maki()
            .current_dir(&root)
            .args(["search", "type:image", "--format", "ids"])
            .assert()
            .success();

        let search_stdout = String::from_utf8_lossy(&search_output.get_output().stdout);
        let first_id = search_stdout.lines().next().unwrap().trim();
        let short_id = &first_id[..8];

        // Find similar
        maki()
            .current_dir(&root)
            .args(["auto-tag", "--similar", short_id])
            .assert()
            .success()
            .stdout(predicate::str::contains("similarity"));
    }

    #[test]
    fn auto_tag_no_results() {
        if !model_available() {
            eprintln!("Skipping auto_tag_no_results: model not downloaded");
            return;
        }

        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        // No assets to tag
        maki()
            .current_dir(&root)
            .args(["auto-tag", "type:image", "--json"])
            .assert()
            .success();
    }

    #[test]
    fn auto_tag_skip_non_image() {
        if !model_available() {
            eprintln!("Skipping auto_tag_skip_non_image: model not downloaded");
            return;
        }

        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        // Create a non-image file
        create_test_file(&root, "document.txt", b"hello world");

        maki()
            .current_dir(&root)
            .args(["import", root.join("document.txt").to_str().unwrap()])
            .assert()
            .success();

        let output = maki()
            .current_dir(&root)
            .args(["auto-tag", "*", "--json"])
            .assert()
            .success();

        let stdout = String::from_utf8_lossy(&output.get_output().stdout);
        let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        // Should skip non-image assets
        assert!(parsed["assets_skipped"].as_u64().unwrap() > 0 || parsed["assets_processed"].as_u64().unwrap() == 0);
    }
}

mod auto_stack {
    use super::*;

    #[test]
    fn auto_stack_empty_catalog_dry_run() {
        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        // No embeddings → 0 candidates, 0 clusters. Doesn't need the model
        // downloaded — auto-stack only reads stored embeddings.
        maki()
            .current_dir(&root)
            .args(["auto-stack"])
            .assert()
            .success()
            .stdout(predicate::str::contains("Auto-stack (dry run): 0 clusters, 0 assets, threshold 85%"));
    }

    #[test]
    fn auto_stack_rejects_low_threshold() {
        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        maki()
            .current_dir(&root)
            .args(["auto-stack", "--threshold", "40"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("threshold must be between 50 and 99"));
    }

    #[test]
    fn auto_stack_rejects_min_size_one() {
        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        maki()
            .current_dir(&root)
            .args(["auto-stack", "--min-size", "1"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("minimum cluster size is 2"));
    }

    #[test]
    fn auto_stack_json_output() {
        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        let output = maki()
            .current_dir(&root)
            .args(["auto-stack", "--json"])
            .assert()
            .success();

        let stdout = String::from_utf8_lossy(&output.get_output().stdout);
        let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(parsed["candidates"], 0);
        assert_eq!(parsed["threshold"], 85);
        assert_eq!(parsed["min_size"], 2);
        assert_eq!(parsed["dry_run"], true);
        assert!(parsed["clusters"].is_array());
        assert!(parsed["warnings"].is_array());
    }
}

mod faces_without_models {
    use super::*;

    /// Catalog whose `[ai] model_dir` points at an empty temp path so
    /// the test never sees models a developer may have downloaded.
    fn init_catalog_without_models(dir: &Path) -> PathBuf {
        let root = init_catalog(dir);
        let model_dir = root.join("no-models").to_string_lossy().replace('\\', "/");
        std::fs::write(root.join("maki.toml"), format!("[ai]\nmodel_dir = \"{model_dir}\"\n")).unwrap();
        root
    }

    #[test]
    fn faces_status_reports_models_not_downloaded() {
        let tmp = tempdir().unwrap();
        let root = init_catalog_without_models(tmp.path());

        maki()
            .current_dir(&root)
            .args(["faces", "status"])
            .assert()
            .success()
            .stdout(
                predicate::str::contains("Models downloaded: no")
                    .and(predicate::str::contains("Total faces detected: 0"))
                    .and(predicate::str::contains("Total people: 0")),
            );

        let out = maki()
            .current_dir(&root)
            .args(["--json", "faces", "status"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let stdout = String::from_utf8(out).unwrap();
        // The JSON document is the last thing on stdout (after the
        // human-readable lines).
        let json_start = stdout.find('{').expect("JSON object on stdout");
        let parsed: serde_json::Value = serde_json::from_str(&stdout[json_start..]).expect("valid JSON");
        assert_eq!(parsed["models_downloaded"], false);
        assert!(parsed["model_dir"].as_str().unwrap().ends_with("faces"));
        assert!(!parsed["current_recognition_model"].as_str().unwrap().is_empty());
        assert_eq!(parsed["total_faces"], 0);
        assert_eq!(parsed["total_people"], 0);
        assert_eq!(parsed["faces_by_model"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn faces_detect_without_models_fails_gracefully() {
        let tmp = tempdir().unwrap();
        let root = init_catalog_without_models(tmp.path());
        let ids = import_and_get_ids(&root, &["face.jpg"]);

        maki()
            .current_dir(&root)
            .args(["faces", "detect", "--asset", &ids[0]])
            .assert()
            .failure()
            .stderr(predicate::str::contains("Face models not downloaded"));

        // Nothing was written: the asset is untouched and still clean.
        maki()
            .current_dir(&root)
            .arg("doctor")
            .assert()
            .success()
            .stdout(predicate::str::contains("no issues found"));
    }
}

#[cfg(feature = "pro")]
mod describe_without_vlm {
    use super::*;

    #[test]
    fn describe_check_unreachable_endpoint_fails_gracefully() {
        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());

        let out = maki()
            .current_dir(&root)
            .args(["--json", "describe", "--check", "--endpoint", "http://127.0.0.1:1", "--timeout", "2"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("vLM endpoint check failed"))
            .get_output()
            .stdout
            .clone();
        let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON on stdout");
        assert_eq!(parsed["status"].as_str(), Some("error"));
        assert_eq!(parsed["endpoint"].as_str(), Some("http://127.0.0.1:1"));
        assert!(parsed["model"].is_string());
        assert!(!parsed["message"].as_str().unwrap().is_empty());

        // Human-readable form: message on stderr, non-zero exit, no panic.
        maki()
            .current_dir(&root)
            .args(["describe", "--check", "--endpoint", "http://127.0.0.1:1", "--timeout", "2"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("panicked").not());
    }

    #[test]
    fn describe_without_scope_errors_before_contacting_vlm() {
        let tmp = tempdir().unwrap();
        let root = init_catalog(tmp.path());
        maki()
            .current_dir(&root)
            .args(["describe", "--endpoint", "http://127.0.0.1:1"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("No scope specified"));
    }
}
