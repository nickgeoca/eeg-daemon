use eeg_driver::{AdcConfig, EegSystem};
use tokio::sync::broadcast;
use warp::ws::{Message, WebSocket};
use warp::Filter;
use serde::Serialize;
use futures_util::{StreamExt, SinkExt};

#[derive(Clone, Serialize)]
struct EegData {
    channels: Vec<f32>,
    timestamp: u64,
}

#[derive(Clone, Serialize)]
struct EegBatchData {
    channels: Vec<Vec<f32>>,  // Each inner Vec represents a channel's data for the batch
    timestamp: u64,           // Timestamp for the start of the batch
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Increase channel capacity but not too much to avoid excessive buffering
    let (tx, _) = broadcast::channel::<EegBatchData>(32);  // Reduced from 1024
    let tx_ws = tx.clone();

    // Create the ADC configuration
    let config = AdcConfig {
        sample_rate: 250,
        channels: vec![0, 1, 2, 3],
        gain: 24.0,
        mock: true,  // Set to false when using real hardware
    };

    println!("Starting EEG system...");
    
    // Create and start the EEG system
    let (mut eeg_system, mut data_rx) = EegSystem::new(config.clone()).await?;
    eeg_system.start(config).await?;

    println!("EEG system started. Waiting for data...");

    // Set up WebSocket route
    let ws_route = warp::path("eeg")
        .and(warp::ws())
        .and(warp::any().map(move || tx_ws.subscribe()))
        .map(|ws: warp::ws::Ws, mut rx: broadcast::Receiver<EegBatchData>| {
            ws.on_upgrade(move |socket| handle_websocket(socket, rx))
        });

    println!("WebSocket server starting on ws://localhost:8080/eeg");

    // Spawn WebSocket server
    let server_handle = tokio::spawn(warp::serve(ws_route).run(([127, 0, 0, 1], 8080)));

    // Process EEG data
    let processing_handle = tokio::spawn(async move {
        let mut count = 0;
        let mut last_time = std::time::Instant::now();
        let mut last_timestamp = None;
        
        while let Some(data) = data_rx.recv().await {
            // Create smaller batches to send more frequently
            // Split the incoming data into chunks of 32 samples
            let batch_size = 32;
            let num_channels = data.data.len();
            let samples_per_channel = data.data[0].len();
            
            for chunk_start in (0..samples_per_channel).step_by(batch_size) {
                let chunk_end = (chunk_start + batch_size).min(samples_per_channel);
                let mut chunk_channels = Vec::with_capacity(num_channels);
                
                for channel in &data.data {
                    chunk_channels.push(channel[chunk_start..chunk_end].to_vec());
                }
                
                let chunk_timestamp = data.timestamp + (chunk_start as u64 * 4000); // Adjust timestamp for each chunk
                
                let eeg_batch_data = EegBatchData {
                    channels: chunk_channels,
                    timestamp: chunk_timestamp / 1000, // Convert to milliseconds
                };
                
                if let Err(e) = tx.send(eeg_batch_data) {
                    println!("Warning: Failed to send data chunk to WebSocket clients: {}", e);
                }
            }
            
            count += data.data[0].len();
            last_timestamp = Some(data.timestamp);
            
            if let Some(last_ts) = last_timestamp {
                let delta_us = data.timestamp - last_ts;
                let delta_ms = delta_us as f64 / 1000.0;  // Convert to milliseconds for display
                if delta_ms > 5.0 {
                    println!("Large timestamp gap detected: {:.2}ms ({} µs)", delta_ms, delta_us);
                    println!("Sample count: {}", count);
                    println!("Expected time between batches: {:.2}ms", (32_000.0 / 250.0)); // For 32 samples at 250Hz
                }
            }
            
            // Print stats every 250 samples (about 1 second of data at 250Hz)
            if count % 250 == 0 {
                let elapsed = last_time.elapsed();
                let rate = 250.0 / elapsed.as_secs_f32();
                println!("Processing rate: {:.2} Hz", rate);
                println!("Total samples processed: {}", count);
                println!("Sample data (first 5 values from first channel):");
                println!("  Channel 0: {:?}", &data.data[0][..5]);
                last_time = std::time::Instant::now();
            }
        }
    });

    // Wait for tasks to complete
    tokio::select! {
        _ = processing_handle => println!("Processing task completed"),
        _ = server_handle => println!("Server task completed"),
    }

    // Cleanup
    eeg_system.stop().await?;
    
    Ok(())
}

async fn handle_websocket(ws: WebSocket, mut rx: broadcast::Receiver<EegBatchData>) {
    let (mut tx, _) = ws.split();
    
    while let Ok(eeg_batch_data) = rx.recv().await {
        if let Ok(msg) = serde_json::to_string(&eeg_batch_data) {
            if let Err(_) = tx.send(Message::text(msg)).await {
                break; // Client disconnected
            }
        }
    }
}
