use chrono::{DateTime, Utc};
use config::Config;

use reporting::{collect_and_write_metrics, send_metrics};
use reqwest::multipart::{Form, Part};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::env;
use std::fs::File;
use std::io::Read;
use std::str;
use std::{boxed::Box, error::Error, path::Path};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::signal::unix::{signal, SignalKind};
use tokio::time::{self, Duration};
use util::{ set_display, Apikey, Updated, Video};
use uuid::Uuid;

mod config;

mod reporting;
mod util;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    set_display();
    let mut config = Config::new();
    let client = Client::new();

    // Load the configs
    config.load().await?;

    let _ = wait_for_api(&client, &config).await?;

    println!("API key is not set. Requesting a new API key...");
    config.key = Some(get_new_key(&client, &mut config).await?.key);
    config.write().await?;



    let mut interval = time::interval(Duration::from_secs(20));
    let mut metrics_interval = time::interval(Duration::from_secs(30));
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut hup = signal(SignalKind::hangup())?;


    loop {
        tokio::select! {
            _ = metrics_interval.tick() => {
                let metrics = collect_and_write_metrics(&config.id).await;
                println!("Running Metrics");
                send_metrics(&config.id, &metrics, &config.key.as_ref().unwrap_or(&String::new()), &config);

                // Check client actions
                let actions = get_client_actions(&client, &config).await;
                if let Some(actions) = actions {
                    if actions.restart_app {
                        restart_app(&client, &config).await;
                    }
                    if actions.restart {
                        restart_device(&client, &config).await;
                    }
                    if actions.screenshot {
                        if let Err(e) = take_screenshot(&client, &config).await {
                            eprintln!("Failed to take screenshot: {}", e);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

async fn wait_for_api(client: &Client, config: &Config) -> Result<bool, Box<dyn Error>> {
    let mut interval = time::interval(Duration::from_secs(1));
    loop {
        let res = client.get(format!("{}/health", config.url)).send().await;
        if let Ok(response) = res {
            match response.status() {
                StatusCode::OK => break,
                StatusCode::INTERNAL_SERVER_ERROR => {
                    println!("Server error. Retrying in 2 minutes...");
                    time::interval(Duration::from_secs(120)).tick().await;
                }
                _ => (),
            }
        }
        interval.tick().await;
    }
    Ok(true)
}

async fn get_new_key(client: &Client, config: &mut Config) -> Result<Apikey, Box<dyn Error>> {
    println!("Loading configuration...");
    config.load().await?;
    println!("{}/get-new-key/{}", config.url, config.id);

    let res: Apikey = client
        .get(format!("{}/get-new-key/{}", config.url, config.id))
        .basic_auth(&config.username, Some(&config.password))
        .send()
        .await?
        .json()
        .await?;

    println!("Received new API key: {}", res.key);
    config.key = Some(res.key.clone());
    config.write().await?;
    Ok(res)
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ClientActions {
    pub client_id: Uuid,
    pub restart_app: bool,
    pub restart: bool,
    pub screenshot: bool,
}

async fn get_client_actions(client: &Client, config: &Config) -> Option<ClientActions> {
    let res = client
        .get(format!("{}/client-actions/{}", config.url, config.id))
        .header("APIKEY", config.key.clone().unwrap_or_default())
        .send()
        .await
        .ok()?;

    if res.status().is_success() {
        res.json::<ClientActions>().await.ok()
    } else {
        println!("Failed to retrieve client actions: {:?}", res.status());
        None
    }
}

async fn restart_app(client: &Client, config: &Config) {
    // Update the restart flag to false
    let update_result = update_restart_app_flag(client, config).await;

    if let Err(e) = update_result {
        println!("Failed to update restart flag: {}", e);
        return;
    }

    println!("Restarting Signage Application...");

    // Stop the MPV player
    let stop_mpv_output = Command::new("pkill").arg("mpv").output().await;

    match stop_mpv_output {
        Ok(output) if output.status.success() => {
            println!("MPV player stopped successfully.");
        }
        Ok(output) => {
            eprintln!(
                "Failed to stop MPV player: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(e) => {
            eprintln!("Failed to execute stop MPV command: {}", e);
        }
    }

    // Restart the signaged.service
    let restart_service_output = Command::new("sudo")
        .arg("systemctl")
        .arg("restart")
        .arg("signaged.service")
        .output()
        .await;

    match restart_service_output {
        Ok(output) if output.status.success() => {
            println!("Signage service restarted successfully.");
        }
        Ok(output) => {
            eprintln!(
                "Failed to restart signage service: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        Err(e) => {
            eprintln!("Failed to execute restart command: {}", e);
            return;
        }
    }
}

async fn update_restart_app_flag(
    client: &Client,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!("{}/update-restart-app-device/{}", config.url, config.id);
    println!("Updating restart app flag at URL: {}", url);
    let response = client
        .post(&url)
        .header("APIKEY", config.key.clone().unwrap_or_default())
        .json(&serde_json::json!({ "restart_app": false }))
        .send()
        .await?;

    if response.status().is_success() {
        println!("Restart App flag successfully updated.");
        Ok(())
    } else {
        Err(format!("Failed to update restart flag: {:?}", response.status()).into())
    }
}

async fn restart_device(client: &Client, config: &Config) {
    // Update the restart flag to false
    let update_result = update_restart_flag(client, config).await;

    if let Err(e) = update_result {
        println!("Failed to update restart flag: {}", e);
        return;
    }

    println!("Restarting device...");
    let status = Command::new("sudo").arg("reboot").status().await;

    match status {
        Ok(status) if status.success() => println!("Device is restarting..."),
        Ok(status) => println!("Failed to restart device, exit code: {}", status),
        Err(e) => println!("Failed to execute reboot command: {}", e),
    }
}

async fn update_restart_flag(
    client: &Client,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!("{}/update-restart-device/{}", config.url, config.id);
    println!("Updating screenshot flag at URL: {}", url);
    let response = client
        .post(&url)
        .header("APIKEY", config.key.clone().unwrap_or_default())
        .json(&serde_json::json!({ "restart": false }))
        .send()
        .await?;

    if response.status().is_success() {
        println!("Restart flag successfully updated.");
        Ok(())
    } else {
        Err(format!("Failed to update restart flag: {:?}", response.status()).into())
    }
}

async fn take_screenshot(client: &Client, config: &Config) -> Result<(), Box<dyn Error>> {
    env::set_var("DISPLAY", ":0");
    env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");

    let screenshot_path = "/home/pi/screenshot.png";

    // Get the screen resolution dynamically using `xrandr`
    let resolution_output = Command::new("xrandr")
        .arg("--current")
        .output()
        .await
        .expect("Failed to execute xrandr");

    let resolution_str = std::str::from_utf8(&resolution_output.stdout)?;
    let resolution_line = resolution_str
        .lines()
        .find(|line| line.contains('*'))
        .ok_or("Failed to find resolution line")?;

    let resolution = resolution_line
        .split_whitespace()
        .nth(0)
        .ok_or("Failed to parse resolution")?;

    // Use the resolution in the ffmpeg command
    let output = Command::new("ffmpeg")
        .arg("-f")
        .arg("x11grab")
        .arg("-video_size")
        .arg(resolution)
        .arg("-i")
        .arg(":0.0")
        .arg("-frames:v")
        .arg("1")
        .arg(screenshot_path)
        .output()
        .await?;

    if output.status.success() {
        println!("Screenshot saved to {}", screenshot_path);
        // Call the upload_screenshot function after taking the screenshot
        if let Err(e) = upload_screenshot(client, config, screenshot_path).await {
            eprintln!("Failed to upload screenshot: {}", e);
        }
    } else {
        eprintln!(
            "Failed to take screenshot: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

async fn update_screenshot_flag(client: &Client, config: &Config) -> Result<(), Box<dyn Error>> {
    let url = format!("{}/update-screenshot-device/{}", config.url, config.id);
    println!("Updating screenshot flag at URL: {}", url);
    let response = client
        .post(&url)
        .header("APIKEY", config.key.clone().unwrap_or_default())
        .json(&json!({ "screenshot": false }))
        .send()
        .await?;

    if response.status().is_success() {
        println!("Screenshot flag successfully updated.");
        Ok(())
    } else {
        Err(format!("Failed to update screenshot flag: {:?}", response.status()).into())
    }
}

async fn upload_screenshot(
    client: &Client,
    config: &Config,
    screenshot_path: &str,
) -> Result<(), Box<dyn Error>> {
    let url = format!("{}/upload-screenshot/{}", config.url, config.id);
    println!("Generated URL: {}", url);
    let mut file = File::open(screenshot_path)?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;

    let part = Part::bytes(buffer)
        .file_name("screenshot.png")
        .mime_str("image/png")?;

    let form = Form::new().part("file", part);

    let response = client
        .post(&url)
        .header("APIKEY", config.key.clone().unwrap_or_default())
        .multipart(form)
        .send()
        .await?;

    if response.status().is_success() {
        println!("Screenshot successfully uploaded.");

        // Delete the screenshot file from the device
        if let Err(e) = std::fs::remove_file(screenshot_path) {
            eprintln!("Failed to delete screenshot: {}", e);
        } else {
            println!("Screenshot deleted from device.");
        }

        // Debugging statements to track the execution flow
        println!("Calling update_screenshot_flag...");
        if let Err(e) = update_screenshot_flag(client, config).await {
            eprintln!("Failed to update screenshot flag: {}", e);
        } else {
            println!("Screenshot flag successfully updated.");
        }

        Ok(())
    } else {
        Err(format!("Failed to upload screenshot: {:?}", response.status()).into())
    }
}
