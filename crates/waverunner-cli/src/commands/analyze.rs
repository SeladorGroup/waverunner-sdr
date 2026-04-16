use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;

use wavecore::analysis;
use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::dsp::decoders;
use wavecore::hardware::GainMode;
use wavecore::session::manager::SessionManager;
use wavecore::session::replay::ReplayDevice;
use wavecore::session::{Command, Event, SessionConfig};

use super::parse_frequency;

#[derive(clap::Args)]
pub struct AnalyzeArgs {
    /// Input IQ file (.cf32, .cu8, .wav)
    pub input: String,

    /// Sample rate in S/s (e.g., 2.048M)
    #[arg(short, long, value_parser = parse_frequency, default_value = "2.048M")]
    pub sample_rate: f64,

    /// Center frequency in Hz (e.g., 433.92M)
    #[arg(short, long, value_parser = parse_frequency, default_value = "100M")]
    pub frequency: f64,

    #[command(subcommand)]
    pub action: AnalyzeAction,
}

#[derive(clap::Subcommand)]
pub enum AnalyzeAction {
    /// Measure bandwidth, channel power, ACPR
    Measure,
    /// Detect and analyze bursts/pulses
    Burst {
        /// Threshold above noise floor in dB
        #[arg(short, long, default_value = "10")]
        threshold_db: f32,
    },
    /// Estimate modulation parameters (type, depth, deviation)
    Modulation,
    /// Inspect raw bits from a file (one bit per line or packed bytes)
    Bits {
        /// File containing raw bits
        file: String,
    },
    /// Export spectrum or stats to CSV/JSON
    Export {
        /// Output file path
        #[arg(short, long)]
        output: String,
        /// Format: csv, json, or png
        #[arg(short, long, default_value = "csv")]
        format: String,
    },
}

pub async fn run(args: AnalyzeArgs, _device_index: u32) -> Result<()> {
    if let AnalyzeAction::Bits { file } = &args.action {
        return run_bitstream(file);
    }

    // Open replay file
    let device = ReplayDevice::open(std::path::Path::new(&args.input), args.sample_rate)
        .map_err(|e| anyhow::anyhow!("Failed to open file: {e}"))?;

    let config = SessionConfig {
        schema_version: 1,
        device_index: 0,
        frequency: args.frequency,
        sample_rate: args.sample_rate,
        gain: GainMode::Auto,
        ppm: 0,
        fft_size: 2048,
        pfa: 1e-4,
    };

    let mut registry = DecoderRegistry::new();
    decoders::register_all(&mut registry);

    let (session, event_rx) = SessionManager::new_with_device(config, device, registry)
        .map_err(|e| anyhow::anyhow!("Failed to start session: {e}"))?;

    // Send the analysis command
    let analysis_id = 1u64;
    let request = match &args.action {
        AnalyzeAction::Measure => {
            // Wait for first spectrum to know the FFT size, then measure center
            analysis::AnalysisRequest::MeasureSignal(analysis::measurement::MeasureConfig {
                signal_center_bin: 1024,
                signal_width_bins: 100,
                adjacent_width_bins: 100,
                obw_threshold_db: 26.0,
            })
        }
        AnalyzeAction::Burst { threshold_db } => {
            analysis::AnalysisRequest::AnalyzeBurst(analysis::burst::BurstConfig {
                threshold_db: *threshold_db,
                min_burst_samples: 10,
                sample_rate: args.sample_rate,
            })
        }
        AnalyzeAction::Modulation => {
            analysis::AnalysisRequest::EstimateModulation(analysis::modulation::ModulationConfig {
                sample_rate: args.sample_rate,
                fft_size: 0,
            })
        }
        AnalyzeAction::Export { output, format } => {
            let fmt = match format.as_str() {
                "json" => analysis::export::ExportFormat::Json,
                "png" => analysis::export::ExportFormat::Png,
                "tsv" => analysis::export::ExportFormat::Tsv,
                "csv" => analysis::export::ExportFormat::Csv,
                other => {
                    anyhow::bail!(
                        "Unknown export format '{other}'. Supported: json, csv, tsv, png"
                    );
                }
            };
            analysis::AnalysisRequest::Export(analysis::export::ExportConfig {
                path: PathBuf::from(output),
                format: fmt,
                content: analysis::export::ExportContent::Spectrum {
                    spectrum_db: Vec::new(), // will use latest from session
                    sample_rate: args.sample_rate,
                    center_freq: args.frequency,
                },
            })
        }
        AnalyzeAction::Bits { .. } => unreachable!(),
    };

    println!(
        "Analyzing {} | Rate: {:.3} MS/s | Freq: {:.6} MHz",
        args.input,
        args.sample_rate / 1e6,
        args.frequency / 1e6,
    );

    // Let a few blocks process before sending the analysis command
    let mut blocks_seen = 0u64;
    let timeout = std::time::Duration::from_millis(50);
    let start = Instant::now();

    // Wait for at least 5 blocks of data
    while session.is_running() && blocks_seen < 5 {
        match event_rx.recv_timeout(timeout) {
            Ok(Event::SpectrumReady(_)) => blocks_seen += 1,
            Ok(Event::Error(e)) => eprintln!("Error: {e}"),
            Ok(_) => {}
            Err(_) => {}
        }
        if start.elapsed().as_secs() > 10 {
            anyhow::bail!("Timeout waiting for data");
        }
    }

    // Send the analysis command
    session
        .send(Command::RunAnalysis {
            id: analysis_id,
            request,
        })
        .map_err(|e| anyhow::anyhow!("Failed to send analysis command: {e}"))?;

    // Wait for the result
    let result_timeout = std::time::Duration::from_secs(10);
    let result_start = Instant::now();

    while session.is_running() {
        match event_rx.recv_timeout(timeout) {
            Ok(Event::AnalysisResult { id, result }) if id == analysis_id => {
                print_analysis_result(&result);
                session.send(Command::Shutdown).ok();
                break;
            }
            Ok(Event::Error(e)) => eprintln!("Error: {e}"),
            Ok(_) => {}
            Err(_) => {}
        }
        if result_start.elapsed() > result_timeout {
            anyhow::bail!("Timeout waiting for analysis result");
        }
    }

    Ok(())
}

fn run_bitstream(file: &str) -> Result<()> {
    let contents = std::fs::read_to_string(file)
        .map_err(|e| anyhow::anyhow!("Failed to read bits file: {e}"))?;

    let bits: Vec<u8> = contents
        .chars()
        .filter(|c| *c == '0' || *c == '1')
        .map(|c| if c == '1' { 1 } else { 0 })
        .collect();

    if bits.is_empty() {
        anyhow::bail!("No bits found in file");
    }

    println!("Analyzing {} bits from {}", bits.len(), file);

    let config = analysis::bitstream::BitstreamConfig {
        bits,
        search_patterns: Vec::new(),
    };

    let report = analysis::bitstream::analyze_bitstream(&config);
    print_bitstream_report(&report);

    Ok(())
}

fn print_analysis_result(result: &analysis::AnalysisResult) {
    match result {
        analysis::AnalysisResult::Measurement(r) => {
            println!("\n=== Signal Measurement ===");
            println!(
                "  -3 dB Bandwidth:   {:.1} Hz ({:.3} kHz)",
                r.bandwidth_3db_hz,
                r.bandwidth_3db_hz / 1e3
            );
            println!(
                "  -6 dB Bandwidth:   {:.1} Hz ({:.3} kHz)",
                r.bandwidth_6db_hz,
                r.bandwidth_6db_hz / 1e3
            );
            println!(
                "  Occupied BW:       {:.1} Hz ({:.1}%)",
                r.occupied_bw_hz, r.obw_percent
            );
            println!("  Channel Power:     {:.2} dBFS", r.channel_power_dbfs);
            println!("  ACPR (lower):      {:.2} dBc", r.acpr_lower_dbc);
            println!("  ACPR (upper):      {:.2} dBc", r.acpr_upper_dbc);
            println!("  PAPR:              {:.2} dB", r.papr_db);
            println!("  Freq Offset:       {:.1} Hz", r.freq_offset_hz);
        }
        analysis::AnalysisResult::Burst(r) => {
            println!("\n=== Burst Analysis ===");
            println!("  Bursts detected:   {}", r.burst_count);
            println!("  Mean pulse width:  {:.1} µs", r.mean_pulse_width_us);
            println!("  Pulse width std:   {:.1} µs", r.pulse_width_std_us);
            println!("  Mean PRI:          {:.1} µs", r.mean_pri_us);
            println!("  PRI std:           {:.1} µs", r.pri_std_us);
            println!("  Duty cycle:        {:.1}%", r.duty_cycle * 100.0);
            println!("  Mean burst SNR:    {:.1} dB", r.mean_burst_snr_db);
            for (i, b) in r.bursts.iter().take(10).enumerate() {
                println!(
                    "    Burst {}: start={} end={} dur={:.1}µs peak={:.1}dB",
                    i + 1,
                    b.start,
                    b.end,
                    b.duration_us,
                    b.peak_power_db,
                );
            }
            if r.bursts.len() > 10 {
                println!("    ... and {} more", r.bursts.len() - 10);
            }
        }
        analysis::AnalysisResult::Modulation(r) => {
            println!("\n=== Modulation Estimation ===");
            println!("  Type:              {}", r.modulation_type);
            println!("  Confidence:        {:.0}%", r.confidence * 100.0);
            if let Some(rate) = r.symbol_rate_hz {
                println!("  Symbol rate:       {:.1} baud", rate);
            }
            if let Some(depth) = r.am_depth {
                println!("  AM depth:          {:.1}%", depth * 100.0);
            }
            if let Some(dev) = r.fm_deviation_hz {
                println!("  FM deviation:      {:.1} Hz ({:.3} kHz)", dev, dev / 1e3);
            }
        }
        analysis::AnalysisResult::Comparison(r) => {
            println!("\n=== Spectrum Comparison ===");
            println!("  RMS difference:    {:.2} dB", r.rms_diff_db);
            println!(
                "  Peak difference:   {:.2} dB (bin {})",
                r.peak_diff_db, r.peak_diff_bin
            );
            println!("  Correlation:       {:.4}", r.correlation);
            println!("  New signals:       {}", r.new_signals.len());
            println!("  Lost signals:      {}", r.lost_signals.len());
        }
        analysis::AnalysisResult::Bitstream(r) => {
            print_bitstream_report(r);
        }
        analysis::AnalysisResult::Tracking(snap) => {
            println!("\n=== Tracking Summary ===");
            println!("  Duration:          {:.1}s", snap.summary.duration_secs);
            println!("  SNR (mean):        {:.1} dB", snap.summary.snr_mean);
            println!(
                "  SNR (min/max):     {:.1} / {:.1} dB",
                snap.summary.snr_min, snap.summary.snr_max
            );
            println!("  Power (mean):      {:.1} dBFS", snap.summary.power_mean);
            println!(
                "  Freq drift:        {:.2} Hz/s",
                snap.summary.freq_drift_hz_per_sec
            );
            println!(
                "  Stability:         {:.0}%",
                snap.summary.stability_score * 100.0
            );
        }
        analysis::AnalysisResult::ExportComplete { path, format } => {
            println!("\nExported to: {} (format: {})", path, format);
        }
    }
}

fn print_bitstream_report(r: &analysis::bitstream::BitstreamReport) {
    println!("\n=== Bitstream Analysis ===");
    println!(
        "  Length:            {} bits ({} bytes)",
        r.length,
        r.length / 8
    );
    println!("  1s fraction:       {:.3}", r.ones_fraction);
    println!("  Max run length:    {} bits", r.max_run_length);
    println!("  Entropy/byte:      {:.2} bits", r.entropy_per_byte);
    if let Some(ref enc) = r.encoding_guess {
        println!("  Encoding:          {}", enc);
    }
    if !r.frame_lengths.is_empty() {
        println!("  Frame lengths:     {:?} bits", r.frame_lengths);
    }
    if !r.ascii_strings.is_empty() {
        println!("  ASCII strings:");
        for frag in &r.ascii_strings {
            println!("    @{}: \"{}\"", frag.byte_offset, frag.text);
        }
    }
    if !r.patterns.is_empty() {
        println!("  Patterns:");
        for p in &r.patterns {
            println!(
                "    {} @ bit {} ({} occurrences)",
                p.pattern_hex, p.bit_offset, p.occurrences
            );
        }
    }
    if !r.hex_dump.is_empty() {
        println!("  Hex dump:");
        for line in r.hex_dump.lines().take(16) {
            println!("    {}", line);
        }
    }
}
