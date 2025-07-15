use crate::config::Config;
use crate::util::run_command;
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde::Serialize;
use std::fs::File;
use std::io::Write;
use uuid::Uuid;

pub async fn temp() -> String {
    run_command("sh", &["-c", "cat /sys/class/thermal/thermal_zone0/temp | column -s $'\\t' -t | sed 's/\\(.\\)..$/.\\1/'"]).await.unwrap_or_default()
}

async fn cpu_usage() -> String {
    run_command("sh", &["-c", "top -bn1 | awk '/Cpu/ {print $2}'"])
        .await
        .unwrap_or_default()
}

async fn memory() -> String {
    run_command(
        "sh",
        &[
            "-c",
            "free -h --si | awk '/Mem/ {printf \"%3.1f\", $3/$2*100}'",
        ],
    )
    .await
    .unwrap_or_default()
}

async fn disk_usage() -> String {
    run_command("sh", &["-c", "df --output=pcent / | tr -dc '0-9'"])
        .await
        .unwrap_or_default()
}

async fn swap_usage() -> String {
    run_command(
        "sh",
        &[
            "-c",
            "free -h --si | awk '/Swap/ {printf \"%3.1f\", $3/$2*100}'",
        ],
    )
    .await
    .unwrap_or_default()
}

async fn uptime() -> String {
    run_command("sh", &["-c", "uptime | awk '{print $3}' | tr -d ','"])
        .await
        .unwrap_or_default()
}

async fn mpvstatus() -> String {
    let output = run_command("sh", &["-c", "ps aux | grep -v grep | grep mpv"])
        .await
        .unwrap_or_default();
    if output.is_empty() {
        "not running".to_string()
    } else {
        "running".to_string()
    }
}

async fn chip_architecture() -> String {
    // Try to get architecture from uname -m
    let arch = run_command("sh", &["-c", "uname -m"])
        .await
        .unwrap_or_default()
        .trim()
        .to_string();
    
    // If we got a result, return it
    if !arch.is_empty() {
        return arch;
    }
    
    // Fallback: try to read from /proc/cpuinfo
    let cpuinfo = run_command("sh", &["-c", "cat /proc/cpuinfo | grep 'model name' | head -1 | cut -d: -f2 | tr -d ' '"])
        .await
        .unwrap_or_default()
        .trim()
        .to_string();
    
    if !cpuinfo.is_empty() {
        return cpuinfo;
    }
    
    // Final fallback
    "unknown".to_string()
}

async fn operating_system() -> String {
    // Try to get OS info from /etc/os-release
    let os_info = run_command("sh", &["-c", "cat /etc/os-release | grep PRETTY_NAME | cut -d= -f2 | tr -d '\"'"])
        .await
        .unwrap_or_default()
        .trim()
        .to_string();
    
    if !os_info.is_empty() {
        return os_info;
    }
    
    // Fallback: try lsb_release
    let lsb_info = run_command("sh", &["-c", "lsb_release -d | cut -f2"])
        .await
        .unwrap_or_default()
        .trim()
        .to_string();
    
    if !lsb_info.is_empty() {
        return lsb_info;
    }
    
    // Final fallback: uname -a
    let uname_info = run_command("sh", &["-c", "uname -a"])
        .await
        .unwrap_or_default()
        .trim()
        .to_string();
    
    if !uname_info.is_empty() {
        return uname_info;
    }
    
    "unknown".to_string()
}

#[derive(Serialize)]
pub struct Metrics {
    client_id: String,
    temp: String,
    processes: String,
    memory: String,
    diskusage: String,
    swapusage: String,
    uptime: String,
    mpvstatus: String,
    chip_architecture: String,
    os: String,
}

pub async fn collect_and_write_metrics(client_id: &str) -> Metrics {
    let metrics = Metrics {
        client_id: client_id.to_string(),
        temp: temp().await,
        processes: cpu_usage().await,
        memory: memory().await,
        diskusage: disk_usage().await,
        swapusage: swap_usage().await,
        uptime: uptime().await,
        mpvstatus: mpvstatus().await,
        chip_architecture: chip_architecture().await,
        os: operating_system().await,
    };

    // Serialize metrics to JSON
    let json = serde_json::to_string_pretty(&metrics).expect("Failed to serialize metrics");

    // Write JSON to a file
    let mut file = File::create("metrics.json").expect("Failed to create file");
    file.write_all(json.as_bytes())
        .expect("Failed to write to file");

    // Print to console for verification
    println!("{}", json);

    metrics
}

pub fn send_metrics(client_id: &str, metrics: &Metrics, api_key: &str, config: &Config) {
    // Check if the client_id is a valid UUID
    if let Err(_) = Uuid::parse_str(client_id) {
        println!("Invalid client ID format: {}", client_id);
        return;
    }

    let client = Client::new();
    let url = format!("{}/client_vitals/{}", config.url, client_id);

    // Print the URL for debugging
    println!("Sending metrics to daddy");

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        "Apikey",
        HeaderValue::from_str(api_key).expect("Invalid API key"),
    );

    let res = client
        .post(&url)
        .headers(headers)
        .json(metrics)
        .send()
        .expect("Failed to send metrics");

    let status = res.status();
    if status.is_success() {
        println!("Successfully sent metrics");
    } else {
        let error_text = res
            .text()
            .unwrap_or_else(|_| "Failed to read error text".to_string());
        println!(
            "Failed to send metrics: {:?}\nError: {}",
            status, error_text
        );
    }
}
