mod network;
mod orchestrator;
mod sync;

use anyhow::{Context, Result};
use clap::Parser;
use gstreamer::prelude::*;
use gstreamer::{ElementFactory, Pipeline};
use network::{NetworkManager, NodeRole, NodeState, RecordingCmd};
use orchestrator::{Orchestrator, RecordingEvent};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use sync::TimeSyncManager;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

#[derive(Parser, Debug)]
#[command(name = "record-syncd")]
#[command(about = "Synchronized video recording over ethernet ad hoc network")]
struct Args {
    /// Node role: leader or follower
    #[arg(short, long, value_enum)]
    role: CliNodeRole,

    /// Network interface IP address (optional)
    #[arg(short, long)]
    interface: Option<IpAddr>,

    /// Output video filename
    #[arg(short, long, default_value = "output_h265.mp4")]
    output: String,

    /// Node ID (auto-generated if not provided)
    #[arg(long)]
    node_id: Option<String>,
}

#[derive(Debug, Clone, clap::ValueEnum)]
enum CliNodeRole {
    Leader,
    Follower,
}

impl From<CliNodeRole> for NodeRole {
    fn from(role: CliNodeRole) -> Self {
        match role {
            CliNodeRole::Leader => NodeRole::Leader,
            CliNodeRole::Follower => NodeRole::Follower,
        }
    }
}

struct RecordingPipeline {
    pipeline: Pipeline,
    filename: String,
}

impl RecordingPipeline {
    fn create(filename: String) -> Result<Self> {
        // Initialize GStreamer
        gstreamer::init()?;

        let pipeline = Pipeline::new();

        // Create elements
        let source = ElementFactory::make("nvarguscamerasrc")
            .build()
            .context("Failed to create nvarguscamerasrc")?;

        let capsfilter = ElementFactory::make("capsfilter")
            .build()
            .context("Failed to create capsfilter")?;

        let encoder = ElementFactory::make("nvv4l2h265enc")
            .build()
            .context("Failed to create nvv4l2h265enc")?;

        let parser = ElementFactory::make("h265parse")
            .build()
            .context("Failed to create h265parse")?;

        let muxer = ElementFactory::make("qtmux")
            .build()
            .context("Failed to create qtmux")?;

        let sink = ElementFactory::make("filesink")
            .build()
            .context("Failed to create filesink")?;

        // Set properties
        let caps = gstreamer::Caps::builder("video/x-raw")
            .features(["memory:NVMM"])
            .field("width", 1920)
            .field("height", 1080)
            .field("format", "NV12")
            .field("framerate", gstreamer::Fraction::new(30, 1))
            .build();

        capsfilter.set_property("caps", &caps);
        encoder.set_property("bitrate", 8000000u32);
        sink.set_property("location", &filename);

        // Add elements to pipeline
        pipeline.add_many([&source, &capsfilter, &encoder, &parser, &muxer, &sink])?;

        // Link elements
        source.link(&capsfilter)?;
        capsfilter.link(&encoder)?;
        encoder.link(&parser)?;
        parser.link(&muxer)?;
        muxer.link(&sink)?;

        Ok(Self { pipeline, filename })
    }

    fn start(&self) -> Result<()> {
        println!("Starting recording to: {}", self.filename);
        self.pipeline.set_state(gstreamer::State::Playing)?;

        // Wait for state change
        let (result, current, _pending) =
            self.pipeline.state(gstreamer::ClockTime::from_seconds(5));
        match result {
            Ok(_) => {
                if current != gstreamer::State::Playing {
                    anyhow::bail!("Failed to reach PLAYING state. Current: {:?}", current);
                }
                println!("Recording started");
            }
            Err(err) => {
                anyhow::bail!("Pipeline state query failed: {:?}", err);
            }
        }

        Ok(())
    }

    fn stop(&self) -> Result<()> {
        println!("Stopping recording...");

        // Send EOS to properly finalize the file
        self.pipeline.send_event(gstreamer::event::Eos::new());

        // Wait for EOS
        let bus = self.pipeline.bus().context("Pipeline has no bus")?;
        for msg in bus.iter_timed(gstreamer::ClockTime::from_seconds(5)) {
            use gstreamer::MessageView;
            match msg.view() {
                MessageView::Eos(..) => {
                    println!("EOS received");
                    break;
                }
                MessageView::Error(err) => {
                    eprintln!("Error during EOS: {:?}", err.error());
                    break;
                }
                _ => (),
            }
        }

        self.pipeline.set_state(gstreamer::State::Null)?;
        println!("Recording stopped");

        Ok(())
    }

    fn monitor_bus(&self, running: Arc<AtomicBool>) {
        let bus = self.pipeline.bus().expect("Pipeline has no bus");
        let pipeline_weak = self.pipeline.downgrade();

        std::thread::spawn(move || {
            while running.load(Ordering::SeqCst) {
                if let Some(msg) =
                    bus.timed_pop(gstreamer::ClockTime::from_mseconds(100))
                {
                    use gstreamer::MessageView;
                    match msg.view() {
                        MessageView::Error(err) => {
                            eprintln!(
                                "Error from {:?}: {} ({:?})",
                                err.src().map(|s| s.path_string()),
                                err.error(),
                                err.debug()
                            );
                            running.store(false, Ordering::SeqCst);
                            break;
                        }
                        MessageView::Warning(warn) => {
                            eprintln!(
                                "Warning from {:?}: {} ({:?})",
                                warn.src().map(|s| s.path_string()),
                                warn.error(),
                                warn.debug()
                            );
                        }
                        MessageView::StateChanged(state_changed) => {
                            if let Some(pipeline) = pipeline_weak.upgrade() {
                                if state_changed
                                    .src()
                                    .map(|s| s == &pipeline)
                                    .unwrap_or(false)
                                {
                                    println!(
                                        "Pipeline state: {:?} -> {:?}",
                                        state_changed.old(),
                                        state_changed.current()
                                    );
                                }
                            }
                        }
                        _ => (),
                    }
                }
            }
        });
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Generate node ID if not provided
    let node_id = args.node_id.unwrap_or_else(|| {
        format!("node-{}", std::process::id())
    });

    println!("Starting record-syncd");
    println!("Node ID: {}", node_id);
    println!("Role: {:?}", args.role);
    if let Some(interface) = args.interface {
        println!("Interface: {}", interface);
    }

    let running = Arc::new(AtomicBool::new(true));

    // Set up Ctrl+C handler
    {
        let running = running.clone();
        ctrlc::set_handler(move || {
            println!("\nReceived Ctrl+C, stopping...");
            running.store(false, Ordering::SeqCst);
        })?;
    }

    // Create network and sync components
    let role: NodeRole = args.role.clone().into();
    let network = Arc::new(
        NetworkManager::new(node_id.clone(), role, args.interface)
            .await
            .context("Failed to create network manager")?,
    );
    let time_sync = Arc::new(TimeSyncManager::new(10));

    // Create recording event channel
    let (recording_tx, mut recording_rx) = mpsc::channel::<RecordingEvent>(10);

    // Create orchestrator
    let orchestrator = Arc::new(Orchestrator::new(
        network.clone(),
        time_sync.clone(),
        running.clone(),
        recording_tx,
    ));

    // Start orchestrator in background
    let orchestrator_handle = {
        let orchestrator = orchestrator.clone();
        tokio::spawn(async move {
            if let Err(e) = orchestrator.run().await {
                eprintln!("Orchestrator error: {}", e);
            }
        })
    };

    // Recording state
    let mut recording_pipeline: Option<RecordingPipeline> = None;

    // Leader UI task
    let leader_ui_handle = if matches!(args.role, CliNodeRole::Leader) {
        let orchestrator = orchestrator.clone();
        let running = running.clone();
        
        Some(tokio::spawn(async move {
            println!("\n=== Leader Commands ===");
            println!("  start [delay_ms] - Start recording (default: 2000ms)");
            println!("  stop [delay_ms]  - Stop recording (default: 1000ms)");
            println!("  peers            - List connected peers");
            println!("  quit             - Exit");
            println!("========================\n");

            let stdin = tokio::io::stdin();
            let mut reader = BufReader::new(stdin);
            let mut line = String::new();

            while running.load(Ordering::SeqCst) {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let parts: Vec<&str> = line.trim().split_whitespace().collect();
                        if parts.is_empty() {
                            continue;
                        }

                        match parts[0] {
                            "start" => {
                                let delay = parts.get(1)
                                    .and_then(|s| s.parse::<i64>().ok())
                                    .unwrap_or(2000);
                                
                                if let Err(e) = orchestrator
                                    .send_recording_command(RecordingCmd::Start, delay)
                                    .await
                                {
                                    eprintln!("Failed to send start command: {}", e);
                                }
                            }
                            "stop" => {
                                let delay = parts.get(1)
                                    .and_then(|s| s.parse::<i64>().ok())
                                    .unwrap_or(1000);
                                
                                if let Err(e) = orchestrator
                                    .send_recording_command(RecordingCmd::Stop, delay)
                                    .await
                                {
                                    eprintln!("Failed to send stop command: {}", e);
                                }
                            }
                            "peers" => {
                                let peers = orchestrator.get_peers().await;
                                println!("\nConnected peers:");
                                for (id, role, state) in peers {
                                    println!("  {} - {:?} ({:?})", id, role, state);
                                }
                                println!();
                            }
                            "quit" => {
                                running.store(false, Ordering::SeqCst);
                                break;
                            }
                            _ => {
                                println!("Unknown command: {}", parts[0]);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Error reading input: {}", e);
                        break;
                    }
                }
            }
        }))
    } else {
        None
    };

    // Main event loop
    while running.load(Ordering::SeqCst) {
        tokio::select! {
            Some(event) = recording_rx.recv() => {
                match event {
                    RecordingEvent::Start => {
                        if recording_pipeline.is_none() {
                            match RecordingPipeline::create(args.output.clone()) {
                                Ok(pipeline) => {
                                    if let Err(e) = pipeline.start() {
                                        eprintln!("Failed to start recording: {}", e);
                                        let _ = orchestrator.send_status(NodeState::Error).await;
                                    } else {
                                        pipeline.monitor_bus(running.clone());
                                        recording_pipeline = Some(pipeline);
                                        let _ = orchestrator.send_status(NodeState::Recording).await;
                                    }
                                }
                                Err(e) => {
                                    eprintln!("Failed to create pipeline: {}", e);
                                    let _ = orchestrator.send_status(NodeState::Error).await;
                                }
                            }
                        } else {
                            println!("Already recording");
                        }
                    }
                    RecordingEvent::Stop => {
                        if let Some(pipeline) = recording_pipeline.take() {
                            if let Err(e) = pipeline.stop() {
                                eprintln!("Failed to stop recording: {}", e);
                            }
                            let _ = orchestrator.send_status(NodeState::Idle).await;
                        } else {
                            println!("Not currently recording");
                        }
                    }
                }
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {
                // Periodic tasks can go here
            }
        }
    }

    // Cleanup
    println!("Shutting down...");
    
    // Stop recording if active
    if let Some(pipeline) = recording_pipeline.take() {
        let _ = pipeline.stop();
    }

    // Wait for tasks to complete
    orchestrator_handle.abort();
    if let Some(handle) = leader_ui_handle {
        handle.abort();
    }

    println!("Goodbye!");
    Ok(())
}
