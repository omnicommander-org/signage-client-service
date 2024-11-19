use anyhow::Result;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{boxed::Box, error::Error, path::Path};
use tokio::process::Command;
use tokio::{
    fs::{self, File},
    io::{AsyncReadExt, AsyncWriteExt},
};

use std::env;

#[derive(Serialize, Deserialize, Clone)]
pub struct Apikey {
    pub key: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]


#[derive(Debug, Serialize, Deserialize)]
pub struct Updated {
    pub updated: Option<DateTime<Utc>>,
}

/// Loads json from `dir/filename` into `T`
pub async fn load_json<T: Serialize + DeserializeOwned>(
    json: &mut T,
    dir: &str,
    filename: &str,
) -> Result<(), Box<dyn Error>> {
    if Path::new(&format!("{dir}/{filename}")).try_exists()? {
        let mut file = File::open(format!("{dir}/{filename}")).await?;
        let mut contents = vec![];
        file.read_to_end(&mut contents).await?;
        *json = serde_json::from_slice(&contents)?;
    } else {
        fs::create_dir_all(dir).await?;
        write_json(json, &format!("{dir}/{filename}")).await?;
    }

    Ok(())
}

pub async fn run_command(
    command: &str,
    args: &[&str],
) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new(command).args(args).output().await?;

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Writes json from `T` into `path`
pub async fn write_json<T: Serialize>(json: &T, path: &str) -> Result<(), Box<dyn Error>> {
    let mut file = File::create(path).await?;
    file.write_all(&serde_json::to_vec_pretty(&json)?).await?;

    Ok(())
}

pub fn set_display() {
    // Set the DISPLAY environment variable for the current process
    env::set_var("DISPLAY", ":0");

    // Optionally, print the current environment variable to verify
    match env::var("DISPLAY") {
        Ok(val) => println!("DISPLAY is set to: {}", val),
        Err(e) => println!("Couldn't read DISPLAY: {}", e),
    }
}
