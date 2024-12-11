use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
pub struct ImageMetadata {
    pub hash: String,
    pub filename: String,
    pub status: String,
    pub output_file: Option<String>,
    pub last_updated: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Metadata {
    pub images: Vec<ImageMetadata>,
}

impl Metadata {
    pub fn load(path: &str) -> Result<Metadata> {
        if Path::new(path).exists() {
            let data = fs::read_to_string(path)?;
            let metadata: Metadata = serde_json::from_str(&data)?;
            Ok(metadata)
        } else {
            Ok(Metadata::default())
        }
    }

    pub fn save(&self, path: &str) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }
}
