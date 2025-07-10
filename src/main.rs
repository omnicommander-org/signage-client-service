

use chrono::{DateTime, Utc};
use config::Config;
use data::Data;
use reqwest::{Client, StatusCode};
use std::{boxed::Box, error::Error, path::Path};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::signal::unix::{signal, SignalKind};
use tokio::time::{self, Duration};
use util::{cleanup_directory, set_display, Apikey, Updated, Video};
use uuid::Uuid;

mod config;
mod data;
mod util;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    set_display();
    let mut config = Config::new();
    let mut data = Data::new();
    let client = Client::new();

    // Load the configs
    println!("Loading configuration...");
    config.load().await?;
    println!("Loaded configuration: {:?}", config);
    println!("Loading data...");
    data.load().await?;

    let mut mpv = start_mpv().await?;

    let _ = wait_for_api(&client, &config).await?;

    println!("API key is not set. Requesting a new API key...");
    config.key = Some(get_new_key(&client, &mut config).await?.key);
    config.write().await?;

    // Get the videos if we've never updated
    if data.last_update.is_none() {
        let updated = sync(&client, &config).await?;
        update_videos(&client, &mut config, &mut data, updated).await?;
        println!("Data Updated: {:?}", updated);
    }
    if data.update_content.unwrap_or(false) {
        let updated = sync(&client, &config).await?;
        update_videos(&client, &mut config, &mut data, updated).await?;
        println!("Data Updated: {:?}", updated);
    }
    

    // Initialize with default polling interval
    let mut poll_interval = Duration::from_secs(60);
    let mut interval = time::interval(poll_interval);
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut hup = signal(SignalKind::hangup())?;

    mpv.kill().await?;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                println!("\n=== Checking for updates ===");
                let mut content_updated = false;
                
                // Try new schedule-aware system first
                match check_timeline_schedule(&client, &mut config).await {
                    Ok(schedule_response) => {
                        println!("✅ Using timeline schedule system");
                        
                        // Process the schedule response
                        content_updated = process_schedule_response(
                            &client, 
                            &mut config, 
                            &mut data, 
                            schedule_response.clone()
                        ).await?;
                        
                        // Calculate optimal polling interval based on schedule timing
                        let new_interval = calculate_poll_interval(&schedule_response);
                        if new_interval != poll_interval {
                            poll_interval = new_interval;
                            interval = time::interval(poll_interval);
                            println!("📊 Updated polling interval to {:?}", poll_interval);
                        }
                    }
                    Err(err) => {
                        println!("⚠️ Schedule check failed: {}, falling back to legacy sync", err);
                        
                        // Fall back to legacy sync system
                        let updated = sync(&client, &config).await?;
                        match (updated, data.last_update, data.update_content) {
                            (Some(updated), Some(last_update), _) if updated > last_update => {
                                println!("🔄 Legacy update detected");
                                update_videos(&client, &mut config, &mut data, Some(updated)).await?;
                                content_updated = true;
                            }
                            (Some(updated), None, _) => {
                                println!("🔄 Legacy initial update");
                                update_videos(&client, &mut config, &mut data, Some(updated)).await?;
                                content_updated = true;
                            }
                            _ => {
                                println!("📋 No legacy updates available");
                            }
                        }
                        
                        // Check legacy update_content flag
                        if data.update_content.unwrap_or(false) {
                            let updated = sync(&client, &config).await?;
                            update_videos(&client, &mut config, &mut data, updated).await?;
                            content_updated = true;
                            println!("🔄 Legacy content flag update");
                        }
                        
                        // Use default polling for legacy fallback
                        if poll_interval != Duration::from_secs(20) {
                            poll_interval = Duration::from_secs(20);
                            interval = time::interval(poll_interval);
                            println!("📊 Using legacy polling interval: 20s");
                        }
                    }
                }
                
                if content_updated {
                    println!("✅ Content updated successfully");
                    
                    // Force MPV restart to pick up new playlist immediately
                    println!("🔄 Restarting MPV to load new playlist...");
                    mpv.kill().await?;
                    mpv = start_mpv().await?;
                    println!("🎬 MPV restarted with new playlist");
                } else {
                    println!("📋 No content changes needed");
                }

                // Restart mpv if it exits
                match mpv.try_wait() {
                    Ok(Some(_)) => {
                        mpv = start_mpv().await?;
                        println!("🎬 Restarted mpv process");
                    },
                    Ok(None) => (),
                    Err(error) => eprintln!("❌ Error waiting for mpv process: {error}"),
                }

                // Avoid restarting mpv too frequently
                time::sleep(Duration::from_secs(10)).await;
            }
            _ = terminate.recv() => {
                println!("Received SIGTERM, terminating...");
                mpv.kill().await?;
                break;
            }
            _ = interrupt.recv() => {
                println!("Received SIGINT, terminating...");
                mpv.kill().await?;
                break;
            }
            _ = hup.recv() => {
                println!("Received SIGHUP, reloading configuration...");
                config.load().await?;
                data.load().await?;
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

async fn start_mpv() -> Result<Child, Box<dyn Error>> {
    let image_display_duration = 10;
    let child = Command::new("mpv")
        .arg("--loop-playlist=inf")
        .arg("--volume=-1")
        .arg("--no-terminal")
        .arg("--fullscreen")
        .arg("--input-ipc-server=/tmp/mpvsocket")
        .arg(format!(
            "--image-display-duration={}",
            image_display_duration
        ))
        .arg(format!(
            "--playlist={}/.local/share/signage/playlist.txt",
            std::env::var("HOME")?
        ))
        .spawn()?;

    Ok(child)
}

async fn get_new_key(client: &Client, config: &mut Config) -> Result<Apikey, Box<dyn Error>> {
    println!("Loading configuration...");
    config.load().await?;
    println!("Requesting new key from: {}/get-new-key/{}", config.url, config.id);

    let res = client
        .get(format!("{}/get-new-key/{}", config.url, config.id))
        .send()
        .await?;

    match res.status() {
        StatusCode::OK => {
            let text = res.text().await?;
            let key: Apikey = serde_json::from_str(&text)?;
            Ok(key)
        }
        _ => Err(format!("Failed to get new key: {}", res.status()).into()),
    }
}

async fn sync(client: &Client, config: &Config) -> Result<Option<DateTime<Utc>>, Box<dyn Error>> {
    let url = format!("{}/sync/{}", config.url, config.id);
    let response = client.get(&url).send().await?;
    let text = response.text().await?;
    let res: Updated = serde_json::from_str(&text)?;
    Ok(res.updated)
}

async fn receive_videos(
    client: &Client,
    config: &mut Config,
) -> Result<Vec<Video>, Box<dyn Error>> {
    let url = format!("{}/recieve-videos/{}", config.url, config.id);

    let new_key = get_new_key(client, config).await?;
    let auth_token = new_key.key;
    let response = client
        .get(&url)
        .header("Accept", "application/json")
        .header("Cache-Control", "no-cache")
        .header("Accept-Encoding", "gzip, deflate, br")
        .header("Connection", "keep-alive")
        .header("APIKEY", auth_token)
        .send()
        .await?;

    let status = response.status();
    let text = response.text().await?;
    if status.is_success() {
        println!("Raw response: {}", text);
        let res: Vec<Video> = serde_json::from_str(&text)?;
        Ok(res)
    } else {
        Err(format!("Failed to receive videos: {} - {}", status, text).into())
    }
}

async fn receive_videos_for_playlist(
    client: &Client,
    config: &mut Config,
    playlist_id: Uuid,
) -> Result<Vec<Video>, Box<dyn Error>> {
    let url = format!("{}/playlists/{}/videos", config.url, playlist_id);

    let new_key = get_new_key(client, config).await?;
    let auth_token = new_key.key;
    let response = client
        .get(&url)
        .header("Accept", "application/json")
        .header("Cache-Control", "no-cache")
        .header("Accept-Encoding", "gzip, deflate, br")
        .header("Connection", "keep-alive")
        .header("APIKEY", auth_token)
        .send()
        .await?;

    let status = response.status();
    let text = response.text().await?;
    if status.is_success() {
        println!("Raw playlist response: {}", text);
        let res: Vec<Video> = serde_json::from_str(&text)?;
        Ok(res)
    } else {
        Err(format!("Failed to receive playlist videos: {} - {}", status, text).into())
    }
}

async fn update_videos(
    client: &Client,
    config: &mut Config,
    data: &mut Data,
    updated: Option<DateTime<Utc>>,
) -> Result<(), Box<dyn Error>> {
    data.videos = receive_videos(client, config).await?;
    data.videos.sort_by_key(|v| v.asset_order);

    println!("{:#?}",  data.videos);
    let message = "==========================================================  Reduced";
    println!("{}", message);
    data.last_update = updated;
    data.update_content= Some(false);
    data.write().await?;
    let home = std::env::var("HOME")?;

    if Path::new(&format!("{home}/.local/share/signage/playlist.txt")).try_exists()? {
        tokio::fs::remove_file(format!("{home}/.local/share/signage/playlist.txt")).await?;
    }

    let mut file = tokio::fs::File::create(format!("{home}/.local/share/signage/playlist.txt")).await?;

    for video in &data.videos {
        let line = format!("{home}/.local/share/signage/assets/{}\n", video.asset_name);
        file.write_all(line.as_bytes()).await?;
    }

    file.flush().await?;

    cleanup_directory(&format!("{home}/.local/share/signage/assets/"), &data.videos).await?;

    Ok(())
}

async fn update_videos_for_playlist(
    client: &Client,
    config: &mut Config,
    data: &mut Data,
    playlist_id: Uuid,
) -> Result<(), Box<dyn Error>> {
    data.videos = receive_videos_for_playlist(client, config, playlist_id).await?;
    data.videos.sort_by_key(|v| v.asset_order);

    println!("{:#?}", data.videos);
    let message = "========================================================== Playlist Updated";
    println!("{}", message);
    data.last_update = Some(Utc::now());
    data.update_content = Some(false);
    data.write().await?;
    let home = std::env::var("HOME")?;

    if Path::new(&format!("{home}/.local/share/signage/playlist.txt")).try_exists()? {
        tokio::fs::remove_file(format!("{home}/.local/share/signage/playlist.txt")).await?;
    }

    let mut file = tokio::fs::File::create(format!("{home}/.local/share/signage/playlist.txt")).await?;

    for video in &data.videos {
        let line = format!("{home}/.local/share/signage/assets/{}\n", video.asset_name);
        file.write_all(line.as_bytes()).await?;
    }

    file.flush().await?;

    cleanup_directory(&format!("{home}/.local/share/signage/assets/"), &data.videos).await?;

    Ok(())
}

async fn check_timeline_schedule(
    client: &Client,
    config: &mut Config,
) -> Result<util::ClientTimelineScheduleResponse, Box<dyn Error>> {
    let url = format!("{}/client-timeline-schedule/{}", config.url, config.id);
    
    let new_key = get_new_key(client, config).await?;
    let auth_token = new_key.key;
    
    let response = client
        .get(&url)
        .header("Accept", "application/json")
        .header("Cache-Control", "no-cache")
        .header("Accept-Encoding", "gzip, deflate, br")
        .header("Connection", "keep-alive")
        .header("APIKEY", auth_token)
        .send()
        .await?;

    let status = response.status();
    let text = response.text().await?;
    if status.is_success() {
        let res: util::ClientTimelineScheduleResponse = serde_json::from_str(&text)?;
        Ok(res)
    } else {
        Err(format!("Failed to check timeline schedule: {} - {}", status, text).into())
    }
}

fn playlist_changed(
    current_playlist: Option<Uuid>, 
    schedule_response: &util::ClientTimelineScheduleResponse
) -> (bool, Option<Uuid>) {
    let new_playlist = schedule_response.active_playlist_id
        .as_ref()
        .and_then(|s| s.parse::<Uuid>().ok());
    
    let changed = current_playlist != new_playlist;
    (changed, new_playlist)
}

fn calculate_poll_interval(schedule_response: &util::ClientTimelineScheduleResponse) -> Duration {
    // Base interval
    let base_interval = Duration::from_secs(20);
    
    // If there's a schedule change coming up, poll more frequently
    if let Some(next_starts) = &schedule_response.next_schedule_starts_at {
        if let Ok(next_time) = next_starts.parse::<DateTime<Utc>>() {
            let now = Utc::now();
            let time_until_next = next_time.signed_duration_since(now);
            
            // If next schedule is within 5 minutes, poll every 10 seconds
            if time_until_next.num_minutes() <= 5 {
                return Duration::from_secs(10);
            }
            // If next schedule is within 30 minutes, poll every 30 seconds
            else if time_until_next.num_minutes() <= 30 {
                return Duration::from_secs(30);
            }
        }
    }
    
    // If there's an active schedule ending soon, poll more frequently
    if let Some(ends_at) = &schedule_response.schedule_ends_at {
        if let Ok(end_time) = ends_at.parse::<DateTime<Utc>>() {
            let now = Utc::now();
            let time_until_end = end_time.signed_duration_since(now);
            
            // If current schedule ends within 5 minutes, poll every 10 seconds
            if time_until_end.num_minutes() <= 5 {
                return Duration::from_secs(10);
            }
            // If current schedule ends within 30 minutes, poll every 30 seconds
            else if time_until_end.num_minutes() <= 30 {
                return Duration::from_secs(30);
            }
        }
    }
    
    base_interval
}

/// Process schedule response and update data if needed
async fn process_schedule_response(
    client: &Client,
    config: &mut Config,
    data: &mut Data,
    schedule_response: util::ClientTimelineScheduleResponse,
) -> Result<bool, Box<dyn Error>> {
    let (playlist_changed, new_playlist) = playlist_changed(data.current_playlist, &schedule_response);
    let mut content_updated = false;
    
    // Update data with schedule information
    data.active_schedule_ends = schedule_response.schedule_ends_at;
    data.next_schedule_starts = schedule_response.next_schedule_starts_at;
    data.next_playlist_id = schedule_response.next_playlist_id
        .as_ref()
        .and_then(|s| s.parse::<Uuid>().ok());
    data.fallback_playlist_id = schedule_response.fallback_playlist_id
        .as_ref()
        .and_then(|s| s.parse::<Uuid>().ok());
    
    // Handle playlist changes
    if playlist_changed {
        println!("🔄 Playlist changed from {:?} to {:?}", data.current_playlist, new_playlist);
        data.current_playlist = new_playlist;
        
        if let Some(playlist_id) = new_playlist {
            // Use the new playlist-specific endpoint
            update_videos_for_playlist(client, config, data, playlist_id).await?;
            content_updated = true;
        } else if let Some(fallback_id) = data.fallback_playlist_id {
            // Use fallback playlist if available
            println!("🔄 Using fallback playlist: {}", fallback_id);
            update_videos_for_playlist(client, config, data, fallback_id).await?;
            content_updated = true;
        } else {
            // No active playlist and no fallback - this might be an error condition
            println!("⚠️ No active playlist and no fallback playlist configured");
        }
    }
    
    // Acknowledge any pending update flags
    if let Some(ref flags) = schedule_response.update_flags {
        acknowledge_updates(client, config, flags).await?;
    }
    
    Ok(content_updated)
}

async fn acknowledge_updates(
    client: &Client,
    config: &mut Config,
    _update_flags: &util::ClientUpdateFlagsResponse,
) -> Result<(), Box<dyn Error>> {
    let url = format!("{}/client-update-flags/{}", config.url, config.id);
    
    let new_key = get_new_key(client, config).await?;
    let auth_token = new_key.key;
    
    let ack_payload = serde_json::json!({
        "content_update_needed": false,
        "playlist_update_needed": false,
        "schedule_update_needed": false
    });
    
    let response = client
        .post(&url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("APIKEY", auth_token)
        .json(&ack_payload)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let text = response.text().await?;
        println!("⚠️ Failed to acknowledge updates: {} - {}", status, text);
    } else {
        println!("✅ Acknowledged update flags");
    }
    
    Ok(())
}
