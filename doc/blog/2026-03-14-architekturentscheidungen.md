---
layout: single
title: "Architekturentscheidungen: Wie man Claude bei Design-Fragen führt statt folgt"
date: 2026-03-14
categories:
  - tipps
tags:
  - Agentic Coding
  - AI Coding
  - Claude
  - Claude Code
  - Architektur
  - Software Design
  - Rust
---

Claude Code kann in Minuten hunderte Zeilen funktionierenden Code produzieren. Aber *funktionierend* und *richtig entworfen* sind zwei verschiedene Dinge. Im [DAM-Projekt](/tipps/2026/03/04/dam-erfahrungsbericht/) habe ich gelernt: Bei Architekturentscheidungen muss der Mensch führen. Claude ist ein hervorragender Implementierer — aber kein Architekt.

Dieser Artikel zeigt anhand konkreter Beispiele, wo Claude von sich aus den naheliegenden statt den besten Weg wählt, und wie man das steuert.

## Die Architektur des DAM-Systems

Bevor wir in die Entscheidungen einsteigen, ein kurzer Blick auf die Schichtenarchitektur:

```
┌─────────────────────────────────────────────────────┐
│                  Interface Layer                     │
│  ┌──────────────┐  ┌────────────────────────────┐   │
│  │   CLI (clap) │  │  Web UI (axum + askama)    │   │
│  └──────┬───────┘  └──────────┬─────────────────┘   │
│         │                     │                      │
├─────────┴─────────────────────┴──────────────────────┤
│                   Core Library                       │
│  ┌──────────────┐ ┌──────────────┐ ┌─────────────┐  │
│  │Asset Service │ │Content Store │ │Query Engine │  │
│  │  (business   │ │  (SHA-256,   │ │  (search,   │  │
│  │   logic)     │ │   dedup)     │ │   edit)     │  │
│  └──────────────┘ └──────────────┘ └─────────────┘  │
│  ┌──────────────┐ ┌──────────────┐ ┌─────────────┐  │
│  │Metadata Store│ │Device Reg.   │ │Preview Gen. │  │
│  │  (YAML)      │ │  (Volumes)   │ │  (dcraw,    │  │
│  │              │ │              │ │   ffmpeg)   │  │
│  └──────────────┘ └──────────────┘ └─────────────┘  │
│                                                      │
├──────────────────────────────────────────────────────┤
│                  Storage Layer                       │
│  ┌──────────────────┐  ┌────────────────────────┐   │
│  │  Local Catalog    │  │    Media Volumes       │   │
│  │  (SQLite + YAML)  │  │    (extern/offline)    │   │
│  └──────────────────┘  └────────────────────────┘   │
└─────────────────────────────────────────────────────┘
```

Diese Architektur stand am Anfang — nicht in Code, sondern in einem 30-seitigen Dokument. Claude hat sie als Diskussionspartner mitentwickelt. Aber die Schlüsselentscheidungen kamen von mir.

## Entscheidung 1: Dual Storage statt "nur SQLite"

**Das Problem:** Wohin mit den Metadaten? SQLite ist schnell und bequem. YAML-Dateien sind menschenlesbar und versionierbar.

**Claudes Instinkt:** SQLite für alles. Das ist der Standard-Ansatz, den jedes Tutorial zeigt.

**Meine Entscheidung:** Beides. YAML als Source of Truth, SQLite als abgeleiteter Cache.

```
Schreibvorgang: Edit Rating → 5 Sterne

┌──────────────┐     ┌───────────────────────────────┐
│  QueryEngine │────▶│  1. SQLite UPDATE assets       │
│  .set_rating │     │     SET rating = 5             │
│              │     │                                │
│              │────▶│  2. YAML Sidecar schreiben     │
│              │     │     rating: 5                  │
│              │     │                                │
│              │────▶│  3. XMP Write-back             │
│              │     │     xmp:Rating="5"             │
└──────────────┘     └───────────────────────────────┘
```

**Warum?** Weil ein `rebuild-catalog` jederzeit die SQLite-Datenbank aus den YAML-Dateien neu aufbauen kann. Und weil YAML-Dateien mit `git diff` inspizierbar sind. Die SQLite-Datenbank ist schnell, aber entbehrlich. Die YAML-Dateien sind das, was zählt.

**Was Claude daraus gelernt hat:** Bei jedem neuen Feature, das Daten speichert, fragt sich Claude: "Muss ich das in YAML *und* SQLite schreiben?" Durch die CLAUDE.md weiß es die Antwort: Ja.

## Entscheidung 2: Denormalisierung statt JOINs

**Das Problem:** Die Browse-Seite zeigt eine Grid-Ansicht mit hunderten Asset-Karten. Jede Karte braucht das Vorschaubild des "besten" Variants (Export > Processed > Original). Der naive Ansatz: JOIN über `assets`, `variants`, `file_locations`, dann GROUP BY mit einer komplizierten Ranking-Logik.

**Claudes Instinkt:** Ein SQL-Query mit JOINs und Subquery.

```sql
-- Claudes erster Vorschlag (vereinfacht):
SELECT a.*, v.content_hash, v.format
FROM assets a
JOIN variants v ON v.asset_id = a.id
WHERE v.content_hash = (
    SELECT content_hash FROM variants
    WHERE asset_id = a.id
    ORDER BY role_priority, format_priority, file_size DESC
    LIMIT 1
)
```

**Meine Entscheidung:** Denormalisierung. Drei zusätzliche Spalten auf der `assets`-Tabelle:

```sql
ALTER TABLE assets ADD COLUMN best_variant_hash TEXT;
ALTER TABLE assets ADD COLUMN primary_variant_format TEXT;
ALTER TABLE assets ADD COLUMN variant_count INTEGER DEFAULT 0;
```

Diese werden bei jedem Schreibvorgang aktualisiert:

```
Denormalisierte Spalten — Update-Pfade:

  insert_asset()                    ──▶ berechne best_variant_hash,
                                        primary_variant_format, variant_count

  update_denormalized_columns()     ──▶ Neuberechnung nach group/ungroup

  fix_roles()                       ──▶ Neuberechnung nach Rollenänderung

  StackStore::set_pick()            ──▶ Neuberechnung nach Stack-Änderung
```

**Das Ergebnis:** Die Browse-Query wird trivial:

```sql
SELECT a.*, v.content_hash FROM assets a
LEFT JOIN variants v ON v.content_hash = a.best_variant_hash
WHERE ...
```

Kein GROUP BY, kein Subquery, kein Ranking — ein einfacher JOIN. Bei 250.000 Assets macht das den Unterschied zwischen 200ms und 20ms Ladezeit.

**Die Lektion:** Claude optimiert lokal — es schreibt den besten Query für das aktuelle Problem. Aber die architektonische Entscheidung, ob man den Aufwand in die Schreibseite (Denormalisierung) oder die Leseseite (Query-Komplexität) verlagert, muss der Mensch treffen.

## Entscheidung 3: Feature Flags statt Monolith

**Das Problem:** AI-Features (SigLIP, Gesichtserkennung) brauchen ONNX Runtime — eine C++-Bibliothek, die den Compile-Vorgang um 2-5 Minuten verlängert und 50-150 MB zur Binary-Größe addiert. Nicht jeder Nutzer braucht das.

**Claudes Instinkt:** Alles einbauen und über Konfiguration steuern.

**Meine Entscheidung:** Cargo Feature Flag.

```toml
# Cargo.toml
[features]
default = []
ai = ["ort", "ndarray", "tokenizers"]
```

```rust
// src/lib.rs
#[cfg(feature = "ai")]
pub mod ai;
#[cfg(feature = "ai")]
pub mod embedding_store;
#[cfg(feature = "ai")]
pub mod face;
```

```rust
// src/web/mod.rs — Route-Registrierung
#[cfg(feature = "ai")]
let app = app
    .route("/api/asset/{id}/suggest-tags",
           axum::routing::post(routes::suggest_tags))
    .route("/api/asset/{id}/similar",
           axum::routing::post(routes::find_similar));
```

**Warum?** Weil `cargo build` in 15 Sekunden durchläuft, wenn man nur am Web-UI arbeitet. `cargo build --features ai` braucht 3 Minuten. Während der täglichen Entwicklung ist das ein massiver Unterschied.

**Claude hat das akzeptiert**, aber nicht vorgeschlagen. Es hätte die AI-Module einfach als reguläre Imports eingebaut. Die Entscheidung, die Build-Pipeline sauber zu halten, kam von mir.

## Entscheidung 4: Content-Addressable statt pfadbasiert

**Das Problem:** Fotodateien werden zwischen Festplatten verschoben. Ein RAW-File kann auf Volume A importiert, auf Volume B kopiert und auf Volume A gelöscht werden. Welche Identität hat das Asset?

**Claudes erster Ansatz wäre:** Dateipfad als Schlüssel.

**Meine Entscheidung:** SHA-256 Hash als Identität.

```
Dieselbe Datei auf drei Pfaden = ein Variant

  /Volumes/Archive/2024/DSC_001.NEF  ──┐
  /Volumes/Backup/Photos/DSC_001.NEF  ──┼──▶  sha256:a1b2c3...
  /Volumes/Working/DSC_001.NEF        ──┘       (eine Identität)
```

Das hat weitreichende Konsequenzen:

```
Content-Addressable Storage — Implikationen:

  ✓ Duplikaterkennung      → gleicher Hash = gleiche Datei
  ✓ Integritätsprüfung     → Hash stimmt nicht? → Datei korrupt
  ✓ Verschiebungserkennung → Hash bekannt, neuer Pfad → Umzug
  ✓ Multi-Volume-Tracking  → ein Variant, viele Locations
  ✓ Offline-Browsing       → Katalog braucht keine Dateien
```

Diese Entscheidung wurde am Tag 1 getroffen, in der initialen Architekturdiskussion mit Claude. Sie ist fundamental — fast jedes Feature baut darauf auf. Claude hat das Konzept verstanden und korrekt umgesetzt. Aber die Entscheidung *für* Content-Addressable Storage über pfadbasierte Identifikation war eine Design-Entscheidung, die Domänenwissen erforderte.

## Entscheidung 5: Bedingte JOINs in der Suche

**Das Problem:** Die Suchfunktion unterstützt über 20 verschiedene Filter: Text, Tags, Rating, Label, Kamera, Objektiv, ISO, Format, Volume, Pfad, Datum, GPS, Stacks, Collections, und diverse Health-Checks. Nicht jede Suche braucht alle Tabellen.

**Claudes Instinkt:** Alle Tabellen immer JOINen. Sicher ist sicher.

**Meine Entscheidung:** Bedingte JOINs.

```rust
// Rückgabe von build_search_where():
struct SearchParts {
    where_clause: String,
    params: Vec<Box<dyn ToSql>>,
    needs_fl_join: bool,    // file_locations benötigt?
    needs_v_join: bool,     // variants benötigt?
}
```

```
Suchquery-Aufbau:

  Einfache Suche: "sunset"
  ───────────────────────────
  SELECT ... FROM assets a
  WHERE a.name LIKE '%sunset%'
  ───▶ Kein JOIN, nur assets-Tabelle

  Format-Filter: "format:nef sunset"
  ───────────────────────────────────
  SELECT ... FROM assets a
  JOIN variants v ON v.asset_id = a.id
  WHERE v.format = 'nef'
    AND a.name LIKE '%sunset%'
  ───▶ JOIN nur auf variants

  Pfad-Filter: "path:Capture/2024"
  ─────────────────────────────────
  SELECT ... FROM assets a
  JOIN variants v ON v.content_hash = a.best_variant_hash
  JOIN file_locations fl ON fl.variant_hash = v.content_hash
  WHERE fl.relative_path LIKE 'Capture/2024%'
  ───▶ JOINs auf variants + file_locations
```

Claude hat das implementiert, nachdem ich die Struktur vorgegeben hatte. Aber die Entscheidung, die Query-Komplexität dynamisch an die Filter anzupassen statt immer alle Tabellen zu verknüpfen, war eine Performance-Entscheidung auf Architekturebene.

## Entscheidung 6: Schema-Migration ohne Framework

**Das Problem:** SQLite-Schema muss sich mit dem Projekt weiterentwickeln. Neue Spalten, neue Tabellen, neue Indizes.

**Claudes Instinkt:** Ein Migration-Framework einbinden.

**Meine Entscheidung:** Idempotente `ALTER TABLE`-Statements.

```rust
// Einfach, robust, kein Framework nötig:
let _ = conn.execute_batch(
    "ALTER TABLE assets ADD COLUMN latitude REAL"
);
// Ignoriert den Fehler, wenn die Spalte schon existiert

// Backfill mit Guard:
conn.execute_batch(
    "UPDATE assets SET latitude = ... WHERE latitude IS NULL"
);
```

**Warum?** Weil das System kein Server ist, der kontrolliert deployed wird. Es ist eine Desktop-Applikation, die auf Dutzenden Rechnern mit verschiedenen Catalog-Versionen läuft. Idempotente Migrationen bedeuten: Egal welche Version der Catalog hat — nach `initialize()` ist er auf dem neuesten Stand. Kein Versions-Tracking, keine Migration-Tabelle, keine Rollbacks.

## Die Rolle des Plan Mode

Claude Code hat einen "Plan Mode": Statt sofort Code zu schreiben, analysiert Claude zunächst den bestehenden Code und skizziert die geplanten Änderungen. Für Architekturentscheidungen ist das unverzichtbar.

```
Typischer Ablauf bei einem neuen Feature:

  1. Ich beschreibe das Feature
       ↓
  2. Claude wechselt in Plan Mode
       ↓
  3. Claude analysiert betroffene Dateien
       ↓
  4. Claude schlägt Änderungen vor
       ↓
  5. Ich prüfe den Architektur-Fit     ←── Hier entscheide ich
       ↓                                     • Passt das zum Dual Storage?
  6. Ggf. Korrektur ("Denormalisiere")     • Sind die JOINs bedingt?
       ↓                                     • Feature-Flag nötig?
  7. Claude implementiert                   • Migrations idempotent?
       ↓
  8. Tests laufen, CLAUDE.md wird aktualisiert
```

Der Plan Mode gibt mir die Möglichkeit, Architekturentscheidungen *vor* der Implementierung zu beeinflussen — nicht nachher, wenn 400 Zeilen Code umgeschrieben werden müssten.

## Muster erkennen: Wann muss der Mensch eingreifen?

Nach 17 Tagen Pair Programming habe ich ein Muster erkannt:

```
Claude trifft gute Entscheidungen bei:
──────────────────────────────────────
✓ Lokale Implementierungsdetails
✓ API-Design (Routen, Parameter)
✓ Test-Struktur und -Abdeckung
✓ Fehlerbehandlung innerhalb eines Moduls
✓ Konsistenz mit bestehenden Patterns

Der Mensch muss eingreifen bei:
──────────────────────────────────────
✗ Trade-offs mit langfristigen Auswirkungen
✗ Performance vs. Einfachheit
✗ Build-Pipeline und Compile-Zeiten
✗ Datenmodell-Entscheidungen
✗ Cross-Cutting Concerns (Dual Storage, Feature Flags)
```

Das ist kein Mangel von Claude — es ist eine Frage des Kontexts. Claude sieht den Code. Der Mensch sieht das System über Wochen und Monate hinweg. Architekturentscheidungen erfordern diesen langfristigen Blick.

## Fazit

Die beste Strategie bei Architekturentscheidungen: **Claude vorschlagen lassen, dann menschliches Urteil anwenden.** Der Plan Mode ist dafür ideal — man sieht den vorgeschlagenen Ansatz, bevor er in Code gegossen wird.

Die wichtigsten Interventionspunkte:
1. **Datenmodell** — Dual Storage, Content-Addressable, Denormalisierung
2. **Build-Pipeline** — Feature Flags, optionale Abhängigkeiten
3. **Performance-Strategie** — Bedingte JOINs, Caching, Denormalisierung
4. **Migration** — Idempotent, ohne Framework, resilient

In jedem dieser Fälle hätte Claude einen funktionierenden, aber suboptimalen Weg gewählt. Die Korrektur dauert 30 Sekunden — das falsche Design sechs Stunden zu reparieren.

Im [nächsten Artikel](/tipps/2026/03/19/testing-und-qualitaet/) geht es darum, wie automatisierte Tests den AI-Workflow absichern — und warum Rust als Sprache dabei ein besonderer Vorteil ist.

---

*Thomas Herrmann ist Geschäftsführer der [42ways GmbH](https://42ways.de) und beschäftigt sich mit dem praktischen Einsatz von KI in der Softwareentwicklung.*
