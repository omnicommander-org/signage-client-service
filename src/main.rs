use config::Config;
use reporting::{collect_and_write_metrics, send_metrics};
use reqwest::{multipart::{Form, Part}, Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{boxed::Box, error::Error};
use std::fs::File;
use std::env;
use tokio::process::Command;
use tokio::signal::unix::{signal, SignalKind};
use tokio::time::{self, Duration};
use uuid::Uuid;

mod config;
mod reporting;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut config = Config::new();
    let client = Client::new();

    // Load the configs
    config.load().await?;

    // Wait for API key
    while config.key.is_none() || config.key.as_ref().unwrap().is_empty() {
        println!("API key not found in configuration. Retrying in 5 seconds...");
        time::sleep(Duration::from_secs(5)).await;
        config.load().await?;
    }

    println!("Using existing API key: {}", config.key.as_ref().unwrap());

    wait_for_api(&client, &config).await?;

    let mut metrics_interval = time::interval(Duration::from_secs(30));
    let mut hup = signal(SignalKind::hangup())?;

    loop {
        tokio::select! {
            _ = metrics_interval.tick() => {
                let metrics = collect_and_write_metrics(&config.id).await;
                println!("Running Metrics");
                send_metrics(&config.id, &metrics, &config.key.as_ref().unwrap(), &config);

                // Check client actions
                if let Some(actions) = get_client_actions(&client, &config).await {
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
            _ = hup.recv() => {
                println!("Received SIGHUP, reloading configuration...");
                config.load().await?;
            }
        }
    }
}

async fn wait_for_api(client: &Client, config: &Config) -> Result<(), Box<dyn Error>> {
    let mut interval = time::interval(Duration::from_secs(1));
    loop {
        let res = client.get(format!("{}/health", config.url)).send().await;
        if let Ok(response) = res {
            if response.status() == StatusCode::OK {
                break;
            }
        }
        interval.tick().await;
    }
    Ok(())
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
    if let Err(e) = update_restart_app_flag(client, config).await {
        println!("Failed to update restart flag: {}", e);
        return;
    }

    println!("Restarting Signage Application...");
    let stop_mpv_output = Command::new("pkill").arg("mpv").output().await;
    if let Err(e) = stop_mpv_output {
        println!("Failed to stop MPV player: {}", e);
    }

    let restart_service_output = Command::new("sudo")
        .arg("systemctl")
        .arg("restart")
        .arg("signaged.service")
        .output()
        .await;

    if let Err(e) = restart_service_output {
        println!("Failed to restart signage service: {}", e);
    }
}

async fn update_restart_app_flag(client: &Client, config: &Config) -> Result<(), Box<dyn Error>> {
    let url = format!("{}/update-restart-app-device/{}", config.url, config.id);
    client
        .post(&url)
        .header("APIKEY", config.key.clone().unwrap_or_default())
        .json(&json!({ "restart_app": false }))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

async fn restart_device(client: &Client, config: &Config) {
    if let Err(e) = update_restart_flag(client, config).await {
        println!("Failed to update restart flag: {}", e);
        return;
    }

    println!("Restarting device...");
    if let Err(e) = Command::new("sudo").arg("reboot").status().await {
        println!("Failed to restart device: {}", e);
    }
}

async fn update_restart_flag(client: &Client, config: &Config) -> Result<(), Box<dyn Error>> {
    let url = format!("{}/update-restart-device/{}", config.url, config.id);
    client
        .post(&url)
        .header("APIKEY", config.key.clone().unwrap_or_default())
        .json(&json!({ "restart": false }))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

async fn take_screenshot(client: &Client, config: &Config) -> Result<(), Box<dyn Error>> {
    env::set_var("DISPLAY", ":0");
    env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");

    let screenshot_path = "/home/pi/screenshot.png";

    let resolution_output = Command::new("xrandr")
        .arg("--current")
        .output()
        .await?;

    let resolution_str = std::str::from_utf8(&resolution_output.stdout)?;
    let resolution_line = resolution_str
        .lines()
        .find(|line| line.contains('*'))
        .ok_or("Failed to find resolution line")?;

    let resolution = resolution_line
        .split_whitespace()
        .nth(0)
        .ok_or("Failed to parse resolution")?;

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
        upload_screenshot(client, config, screenshot_path).await?;
    } else {
        eprintln!(
            "Failed to take screenshot: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

async fn upload_screenshot(
    client: &Client,
    config: &Config,
    screenshot_path: &str,
) -> Result<(), Box<dyn Error>> {
    let url = format!("{}/upload-screenshot/{}", config.url, config.id);
    let mut file = File::open(screenshot_path)?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;

    let part = Part::bytes(buffer)
        .file_name("screenshot.png")
        .mime_str("image/png")?;

    let form = Form::new().part("file", part);

    client
        .post(&url)
        .header("APIKEY", config.key.clone().unwrap_or_default())
        .multipart(form)
        .send()
        .await?
        .error_for_status()?;

    std::fs::remove_file(screenshot_path)?;
    println!("Screenshot uploaded and deleted locally.");
    Ok(())
}
