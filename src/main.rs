mod network;
mod orchestrator;
mod sync;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{info, warn};
use gstreamer::prelude::*;
use gstreamer::{ElementFactory, Pipeline};
use network::{NetworkManager, NodeRole, NodeState, RecordingCmd};
use orchestrator::{Orchestrator, RecordingEvent};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
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

    /// Log file path (optional)
    #[arg(long)]
    log_file: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum RecordingState {
    Idle = 0,
    Recording = 1,
    Stopping = 2,
}

impl RecordingState {
    fn from_u8(val: u8) -> Self {
        match val {
            1 => RecordingState::Recording,
            2 => RecordingState::Stopping,
            _ => RecordingState::Idle,
        }
    }
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
            .field("framerate", gstreamer::Fraction::new(60, 1))
            .build();

        capsfilter.set_property("caps", &caps);
        encoder.set_property("bitrate", 8000000u32);
        
        // Configure muxer to write moov atom at the beginning for better compatibility
        muxer.set_property("faststart", true);
        
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
        info!("Stopping recording pipeline...");

        // Send EOS to flush all data through the pipeline
        self.pipeline.send_event(gstreamer::event::Eos::new());

        // Wait for EOS with extended timeout (up to 60 seconds)
        let bus = self.pipeline.bus().context("Pipeline has no bus")?;
        let start = std::time::Instant::now();
        let max_wait = std::time::Duration::from_secs(60);
        let mut eos_received = false;
        
        info!("Waiting for EOS to finalize video file...");
        while start.elapsed() < max_wait {
            if let Some(msg) = bus.timed_pop(gstreamer::ClockTime::from_mseconds(200)) {
                use gstreamer::MessageView;
                match msg.view() {
                    MessageView::Eos(..) => {
                        info!("EOS received - file finalization complete");
                        eos_received = true;
                        break;
                    }
                    MessageView::Error(err) => {
                        eprintln!("Error during EOS: {:?}", err.error());
                        break;
                    }
                    _ => (),
                }
            }
        }
        
        if !eos_received {
            warn!("EOS timeout - file may not be properly finalized");
        }

        // Transition through states carefully
        info!("Setting pipeline to PAUSED state...");
        self.pipeline.set_state(gstreamer::State::Paused)?;
        std::thread::sleep(std::time::Duration::from_millis(100));
        
        info!("Setting pipeline to NULL state...");
        self.pipeline.set_state(gstreamer::State::Null)?;
        std::thread::sleep(std::time::Duration::from_millis(500));
        
        info!("Recording stopped cleanly");

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



fn init_logging(log_file: Option<String>) -> Result<()> {
    use tracing_subscriber::prelude::*;
    

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stdout)
        .compact();

    if let Some(path) = log_file {
        let file = std::fs::File::create(&path)
            .context(format!("Failed to create log file {}", path))?;
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(file)
            .compact();
        
        tracing_subscriber::registry()
            .with(fmt_layer)
            .with(file_layer)
            .init();
    } else {
        tracing_subscriber::registry()
            .with(fmt_layer)
            .init();
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    // Initialize logging
    init_logging(args.log_file.clone())?;

    // Generate node ID if not provided
    let node_id = args.node_id.unwrap_or_else(|| {
        format!("node-{}", std::process::id())
    });

    info!("Starting record-syncd");
    info!("Node ID: {}", node_id);
    info!("Role: {:?}", args.role);
    if let Some(interface) = args.interface {
        info!("Interface: {}", interface);
    }

    let running = Arc::new(AtomicBool::new(true));

    let recording_state = Arc::new(AtomicU8::new(0));

    // Set up Ctrl+C handler
    {
        let running_sigint = running.clone();
        ctrlc::set_handler(move || {
            println!("\nReceived Ctrl+C, stopping...");
            running_sigint.store(false, Ordering::SeqCst);
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
        let recording_state_ui = recording_state.clone();
        
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
                                let state = RecordingState::from_u8(recording_state_ui.load(Ordering::SeqCst));
                                match state {
                                    RecordingState::Stopping => {
                                        println!("Video is still finalizing... Please wait.");
                                        println!("The application will exit once the file is written.");
                                    }
                                    RecordingState::Recording => {
                                        println!("Recording in progress. Please use 'stop' command first.");
                                    }
                                    RecordingState::Idle => {
                                        running.store(false, Ordering::SeqCst);
                                        break;
                                    }
                                }
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

    // Track recording state: 0=Idle, 1=Recording, 2=Stopping

    // Main event loop
    while running.load(Ordering::SeqCst) {
        tokio::select! {
            Some(event) = recording_rx.recv() => {
                match event {
                    RecordingEvent::Start => {
                        if recording_pipeline.is_none() {
                            recording_state.store(1, Ordering::SeqCst); // Set to Recording
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
                            recording_state.store(2, Ordering::SeqCst); // Set to Stopping
                            info!("Stopping recording - video file is being finalized...");
                            if let Err(e) = pipeline.stop() {
                                eprintln!("Failed to stop recording: {}", e);
                            }
                            recording_state.store(0, Ordering::SeqCst); // Back to Idle
                            info!("Recording stopped cleanly");
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
