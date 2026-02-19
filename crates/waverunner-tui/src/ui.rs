//! UI layout and rendering.
//!
//! Supports three view tabs:
//! - **Standard**: spectrum + waterfall + signal stats + detections + decoder
//! - **Constellation**: spectrum + IQ scatter + PLL/carrier stats
//! - **Statistics**: spectrum + detailed signal statistics table

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Sparkline};

use wavecore::util::{format_freq, format_step};

use crate::app::{App, DspState, InputMode, ViewTab};
use crate::constellation::ConstellationWidget;
use crate::spectrum::{FreqAxisWidget, SpectrumWidget};
use crate::waterfall::WaterfallWidget;

/// Render the full TUI layout, dispatching to the active tab.
pub fn draw(frame: &mut Frame, app: &App) {
    match app.view_tab {
        ViewTab::Standard => draw_standard_tab(frame, app),
        ViewTab::Constellation => draw_constellation_tab(frame, app),
        ViewTab::Statistics => draw_statistics_tab(frame, app),
        ViewTab::Analysis => draw_analysis_tab(frame, app),
    }
}

// ============================================================================
// Standard tab
// ============================================================================

fn draw_standard_tab(frame: &mut Frame, app: &App) {
    let size = frame.area();
    let has_decoder = app.active_decoder.is_some();

    let chunks = if has_decoder {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),           // Header
                Constraint::Percentage(25),      // Spectrum
                Constraint::Percentage(25),      // Waterfall
                Constraint::Min(5),              // Info + Detections
                Constraint::Length(8),           // Decoded Messages
                Constraint::Length(2),           // Keybindings
            ])
            .split(size)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),           // Header
                Constraint::Percentage(30),      // Spectrum
                Constraint::Percentage(30),      // Waterfall
                Constraint::Min(6),              // Info + Detections
                Constraint::Length(0),           // No decoder panel
                Constraint::Length(2),           // Keybindings
            ])
            .split(size)
    };

    draw_header(frame, app, chunks[0]);
    draw_spectrum(frame, app, &app.dsp, chunks[1]);
    draw_waterfall(frame, app, chunks[2]);
    draw_info_panel(frame, app, &app.dsp, chunks[3]);
    if has_decoder {
        draw_decoded(frame, app, chunks[4]);
    }
    draw_keybindings(frame, app, chunks[5]);
}

// ============================================================================
// Constellation tab
// ============================================================================

fn draw_constellation_tab(frame: &mut Frame, app: &App) {
    let size = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),           // Header
            Constraint::Percentage(30),      // Spectrum
            Constraint::Percentage(30),      // Constellation
            Constraint::Min(5),              // PLL/carrier stats
            Constraint::Length(2),           // Keybindings
        ])
        .split(size);

    draw_header(frame, app, chunks[0]);
    draw_spectrum(frame, app, &app.dsp, chunks[1]);
    draw_constellation(frame, app, &app.dsp, chunks[2]);
    draw_pll_stats(frame, app, &app.dsp, chunks[3]);
    draw_keybindings(frame, app, chunks[4]);
}

// ============================================================================
// Statistics tab
// ============================================================================

fn draw_statistics_tab(frame: &mut Frame, app: &App) {
    let size = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),           // Header
            Constraint::Percentage(30),      // Spectrum
            Constraint::Min(8),              // Stats table
            Constraint::Length(2),           // Keybindings
        ])
        .split(size);

    draw_header(frame, app, chunks[0]);
    draw_spectrum(frame, app, &app.dsp, chunks[1]);
    draw_detailed_stats(frame, app, &app.dsp, chunks[2]);
    draw_keybindings(frame, app, chunks[3]);
}

// ============================================================================
// Shared components
// ============================================================================

/// Header bar with frequency, rate, mode, step, decoder, and tab indicator.
fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let freq_str = format_freq(app.frequency);
    let rate_str = format!("{:.3} MS/s", app.sample_rate / 1e6);
    let mode_str = app.demod_mode.label();
    let step_str = format_step(app.step_hz());
    let gain_str = &app.gain;

    let band_str = app
        .frequency_db
        .band_name(app.frequency)
        .unwrap_or("Unknown");

    let mut spans = vec![
        Span::styled(" WaveRunner ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(&freq_str, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled(" ", Style::default()),
        Span::styled(band_str, Style::default().fg(Color::LightYellow)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(&rate_str, Style::default().fg(Color::Green)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(mode_str, Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("Step: {step_str}"), Style::default().fg(Color::Blue)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("Gain: {gain_str}"), Style::default().fg(Color::White)),
    ];

    // Volume indicator (show when demod is active)
    if app.demod_mode != crate::app::DemodMode::Off {
        spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
        let vol_color = if app.volume == 0 {
            Color::Red
        } else {
            Color::Green
        };
        spans.push(Span::styled(
            format!("Vol: {}%", app.volume),
            Style::default().fg(vol_color),
        ));
    }

    if let Some(sq) = app.squelch {
        spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            format!("Sq: {sq:.0} dB"),
            Style::default().fg(Color::Red),
        ));
    }

    // Show active decoder
    if let Some(ref decoder) = app.active_decoder {
        spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            format!("Dec: {}", decoder.to_uppercase()),
            Style::default().fg(Color::LightCyan),
        ));
    }

    // Show active mode
    if let Some(ref status) = app.mode_status {
        spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            status.clone(),
            Style::default().fg(Color::LightYellow),
        ));
    }

    // Tab indicator
    spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
    spans.push(Span::styled(
        format!("[{}]", app.view_tab.label()),
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
    ));

    // Health badge
    let (health_label, health_color) = match app.health {
        wavecore::session::HealthStatus::Normal => ("[OK]", Color::Green),
        wavecore::session::HealthStatus::Warning => ("[WARN]", Color::Yellow),
        wavecore::session::HealthStatus::Critical => ("[CRIT]", Color::Red),
    };
    spans.push(Span::styled(" ", Style::default()));
    spans.push(Span::styled(
        health_label,
        Style::default().fg(health_color).add_modifier(Modifier::BOLD),
    ));

    // Show frequency entry if active
    if let InputMode::FrequencyEntry(text) = &app.input_mode {
        spans.clear();
        spans.push(Span::styled(
            " Frequency: ",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            text.clone(),
            Style::default().fg(Color::White).add_modifier(Modifier::UNDERLINED),
        ));
        spans.push(Span::styled(
            "█",
            Style::default().fg(Color::White),
        ));
        spans.push(Span::styled(
            "  (Enter=confirm, Esc=cancel, suffixes: k/M/G)",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let header = Paragraph::new(Line::from(spans))
        .block(Block::default().borders(Borders::ALL).title(""));

    frame.render_widget(header, area);
}

/// Spectrum panel with peak hold.
fn draw_spectrum(frame: &mut Frame, app: &App, dsp: &DspState, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Spectrum ");

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height < 3 || dsp.spectrum_db.is_empty() {
        return;
    }

    let spec_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(inner);

    let min_db = -100.0_f32;
    let max_db = 0.0_f32;

    let peak_hold = if dsp.peak_hold_db.len() == dsp.spectrum_db.len() {
        Some(dsp.peak_hold_db.as_slice())
    } else {
        None
    };

    let spectrum = SpectrumWidget {
        spectrum: &dsp.spectrum_db,
        min_db,
        max_db,
        noise_floor: Some(dsp.noise_floor_db),
        peak_hold,
    };

    frame.render_widget(spectrum, spec_chunks[0]);

    let freq_axis = FreqAxisWidget {
        center_freq: app.frequency,
        sample_rate: app.sample_rate,
    };
    frame.render_widget(freq_axis, spec_chunks[1]);
}

/// Waterfall panel.
fn draw_waterfall(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Waterfall ");

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height < 1 {
        return;
    }

    let rows = app.waterfall_ordered();
    let row_refs: Vec<&[f32]> = rows.to_vec();

    let waterfall = WaterfallWidget {
        rows: &row_refs,
        min_db: -100.0,
        max_db: 0.0,
    };

    frame.render_widget(waterfall, inner);
}

/// IQ constellation diagram panel.
fn draw_constellation(frame: &mut Frame, _app: &App, dsp: &DspState, area: Rect) {
    let locked_str = if dsp.pll_locked { " LOCKED" } else { "" };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Constellation{locked_str} "));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height < 4 || dsp.constellation_points.is_empty() {
        if inner.height >= 1 && dsp.constellation_points.is_empty() {
            let msg = Paragraph::new(Line::from(Span::styled(
                "  No demod active — enable a demod mode with [m]",
                Style::default().fg(Color::DarkGray),
            )));
            frame.render_widget(msg, inner);
        }
        return;
    }

    // Auto-range from data
    let max_mag = dsp
        .constellation_points
        .iter()
        .map(|(i, q)| i.abs().max(q.abs()))
        .fold(0.1_f32, f32::max);
    let range = (max_mag * 1.2).max(0.1);

    let widget = ConstellationWidget {
        points: &dsp.constellation_points,
        range,
    };

    frame.render_widget(widget, inner);
}

/// PLL/carrier tracking stats panel (constellation tab).
fn draw_pll_stats(frame: &mut Frame, app: &App, dsp: &DspState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ])
        .split(area);

    // Left: PLL state
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Carrier/PLL ");

    let lock_color = if dsp.pll_locked { Color::Green } else { Color::Red };
    let lock_text = if dsp.pll_locked { "LOCKED" } else { "UNLOCKED" };

    let lines = vec![
        Line::from(vec![
            Span::styled(" Lock: ", Style::default().fg(Color::DarkGray)),
            Span::styled(lock_text, Style::default().fg(lock_color).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled(" Freq: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.1} Hz", dsp.pll_frequency_hz),
                Style::default().fg(Color::Yellow),
            ),
        ]),
        Line::from(vec![
            Span::styled("Phase: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.3} rad", dsp.pll_phase_error),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("  AGC: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:+.1} dB", dsp.agc_gain_db),
                Style::default().fg(Color::Yellow),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Pts: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", dsp.constellation_points.len()),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ];

    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, chunks[0]);

    // Right: signal info
    draw_signal_stats(frame, app, dsp, chunks[1]);
}

/// Detailed statistics table (statistics tab).
fn draw_detailed_stats(frame: &mut Frame, app: &App, dsp: &DspState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ])
        .split(area);

    // Left: detailed signal statistics
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Signal Statistics ");

    let stats = &dsp.stats;
    let lines = vec![
        Line::from(vec![
            Span::styled("      RMS: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.1} dBFS", dsp.rms_dbfs),
                color_by_level(dsp.rms_dbfs),
            ),
        ]),
        Line::from(vec![
            Span::styled("    Floor: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.1} dB", dsp.noise_floor_db),
                Style::default().fg(Color::Blue),
            ),
        ]),
        Line::from(vec![
            Span::styled("      SNR: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.1} dB", dsp.snr_db),
                color_by_snr(dsp.snr_db),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Flatness: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.4}", dsp.spectral_flatness),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Variance: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.6}", stats.variance),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("     Peak: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.4}", stats.peak),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("    Crest: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.1} dB", stats.crest_factor_db),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Skewness: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.3}", stats.skewness),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Kurtosis: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.3} (excess: {:.3})", stats.kurtosis, stats.excess_kurtosis),
                Style::default().fg(Color::White),
            ),
        ]),
    ];

    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, chunks[0]);

    // Right: performance metrics + detections
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Performance ");

    let mut lines = vec![];

    if app.blocks_processed > 0 {
        lines.push(Line::from(vec![
            Span::styled("      CPU: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.1}%", app.cpu_load_percent),
                color_by_cpu(app.cpu_load_percent),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Thru-put: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.1} MS/s", app.throughput_msps),
                Style::default().fg(Color::Green),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("   Blocks: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", app.blocks_processed),
                Style::default().fg(Color::White),
            ),
        ]));
        if app.blocks_dropped > 0 {
            lines.push(Line::from(vec![
                Span::styled("  Dropped: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{}", app.blocks_dropped),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
            ]));
        }
    }

    if dsp.agc_gain_db != 0.0 {
        lines.push(Line::from(vec![
            Span::styled("      AGC: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:+.1} dB", dsp.agc_gain_db),
                Style::default().fg(Color::Yellow),
            ),
        ]));
    }

    // Latency breakdown
    if app.latency.total_us > 0 {
        lines.push(Line::from(Span::styled(
            " ── Latency ──",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(vec![
            Span::styled("       DC: ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{} us", app.latency.dc_removal_us), Style::default().fg(Color::White)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("      FFT: ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{} us", app.latency.fft_us), Style::default().fg(Color::White)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("     CFAR: ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{} us", app.latency.cfar_us), Style::default().fg(Color::White)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    Demod: ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{} us", app.latency.demod_us), Style::default().fg(Color::White)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    Total: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} us", app.latency.total_us),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    // Buffer / events info
    if app.buffer_occupancy > 0 || app.events_dropped > 0 {
        lines.push(Line::from(vec![
            Span::styled("   Buffer: ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{}", app.buffer_occupancy), Style::default().fg(Color::White)),
            Span::styled("  EvDrop: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", app.events_dropped),
                if app.events_dropped > 0 {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default().fg(Color::White)
                },
            ),
        ]));
    }

    // Noise classification hint
    let noise_type = if dsp.stats.excess_kurtosis.abs() < 0.5 {
        "Gaussian"
    } else if dsp.stats.excess_kurtosis > 2.0 {
        "Impulsive"
    } else if dsp.stats.excess_kurtosis < -0.5 {
        "Uniform-like"
    } else {
        "Mixed"
    };
    lines.push(Line::from(vec![
        Span::styled("    Noise: ", Style::default().fg(Color::DarkGray)),
        Span::styled(noise_type, Style::default().fg(Color::White)),
    ]));

    lines.push(Line::from(vec![
        Span::styled("  Detects: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}", dsp.detections.len()),
            Style::default().fg(Color::Yellow),
        ),
    ]));

    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, chunks[1]);
}

/// Info + detections split panel (standard tab).
fn draw_info_panel(frame: &mut Frame, app: &App, dsp: &DspState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ])
        .split(area);

    draw_signal_stats(frame, app, dsp, chunks[0]);
    draw_detections(frame, app, dsp, chunks[1]);
}

/// Signal statistics and performance metrics panel.
fn draw_signal_stats(frame: &mut Frame, app: &App, dsp: &DspState, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Signal ");

    let mut lines = vec![
        Line::from(vec![
            Span::styled("  RMS: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.1} dBFS", dsp.rms_dbfs),
                color_by_level(dsp.rms_dbfs),
            ),
        ]),
        Line::from(vec![
            Span::styled("Floor: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.1} dB", dsp.noise_floor_db),
                Style::default().fg(Color::Blue),
            ),
        ]),
        Line::from(vec![
            Span::styled("  SNR: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.1} dB", dsp.snr_db),
                color_by_snr(dsp.snr_db),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Flat: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.4}", dsp.spectral_flatness),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Kurt: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.2}", dsp.stats.excess_kurtosis),
                Style::default().fg(Color::White),
            ),
        ]),
    ];

    // AGC info when demod active
    if dsp.agc_gain_db != 0.0 {
        lines.push(Line::from(vec![
            Span::styled("  AGC: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:+.1} dB", dsp.agc_gain_db),
                Style::default().fg(Color::Yellow),
            ),
        ]));
    }

    // Performance metrics
    if app.blocks_processed > 0 {
        lines.push(Line::from(vec![
            Span::styled("  CPU: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.1}%", app.cpu_load_percent),
                color_by_cpu(app.cpu_load_percent),
            ),
            Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.1} MS/s", app.throughput_msps),
                Style::default().fg(Color::Green),
            ),
        ]));
        if app.blocks_dropped > 0 {
            lines.push(Line::from(vec![
                Span::styled(" Drop: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{}", app.blocks_dropped),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
            ]));
        }
    }

    lines.push(Line::from(vec![
        Span::styled("Blk #: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}", dsp.block_count),
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, area);
}

/// CFAR detections panel.
fn draw_detections(frame: &mut Frame, app: &App, dsp: &DspState, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Detections ({}) ", dsp.detections.len()));

    let max_rows = area.height.saturating_sub(2) as usize;

    let lines: Vec<Line> = dsp
        .detections
        .iter()
        .take(max_rows)
        .map(|det| {
            let freq_abs = app.frequency + det.freq_offset_hz;
            Line::from(vec![
                Span::styled(
                    format!("{:>14}", format_freq(freq_abs)),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("{:+.1} dB", det.power_db),
                    color_by_level(det.power_db),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("SNR {:.1}", det.snr_db),
                    color_by_snr(det.snr_db),
                ),
            ])
        })
        .collect();

    let para = if lines.is_empty() {
        Paragraph::new(vec![Line::from(Span::styled(
            "  No signals detected",
            Style::default().fg(Color::DarkGray),
        ))])
        .block(block)
    } else {
        Paragraph::new(lines).block(block)
    };

    frame.render_widget(para, area);
}

/// Decoded messages panel.
fn draw_decoded(frame: &mut Frame, app: &App, area: Rect) {
    let decoder_name = app
        .active_decoder
        .as_deref()
        .unwrap_or("none")
        .to_uppercase();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " Decoded — {} ({} msgs) ",
            decoder_name,
            app.decoded_messages.len(),
        ));

    let max_rows = area.height.saturating_sub(2) as usize;

    let lines: Vec<Line> = app
        .decoded_messages
        .iter()
        .rev()
        .take(max_rows)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|msg| {
            let decoder_span = Span::styled(
                format!("[{}] ", msg.decoder.to_uppercase()),
                decoder_color(&msg.decoder),
            );
            let summary_span = Span::styled(
                msg.summary.clone(),
                Style::default().fg(Color::White),
            );
            Line::from(vec![decoder_span, summary_span])
        })
        .collect();

    let para = if lines.is_empty() {
        Paragraph::new(vec![Line::from(Span::styled(
            "  Waiting for decoded messages...",
            Style::default().fg(Color::DarkGray),
        ))])
        .block(block)
    } else {
        Paragraph::new(lines).block(block)
    };

    frame.render_widget(para, area);
}

/// Keybinding help bar — context-sensitive to the current tab.
fn draw_keybindings(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled(" [q]", Style::default().fg(Color::Yellow)),
        Span::styled("uit ", Style::default().fg(Color::DarkGray)),
        Span::styled("[j/k]", Style::default().fg(Color::Yellow)),
        Span::styled("tune ", Style::default().fg(Color::DarkGray)),
        Span::styled("[h/l]", Style::default().fg(Color::Yellow)),
        Span::styled("step ", Style::default().fg(Color::DarkGray)),
        Span::styled("[f]", Style::default().fg(Color::Yellow)),
        Span::styled("req ", Style::default().fg(Color::DarkGray)),
        Span::styled("[m]", Style::default().fg(Color::Yellow)),
        Span::styled("ode ", Style::default().fg(Color::DarkGray)),
        Span::styled("[d]", Style::default().fg(Color::Yellow)),
        Span::styled("ecode ", Style::default().fg(Color::DarkGray)),
        Span::styled("[s]", Style::default().fg(Color::Yellow)),
        Span::styled("quelch ", Style::default().fg(Color::DarkGray)),
        Span::styled("[/]", Style::default().fg(Color::Yellow)),
        Span::styled("vol ", Style::default().fg(Color::DarkGray)),
    ];

    if app.view_tab == ViewTab::Analysis {
        spans.extend([
            Span::styled("[a]", Style::default().fg(Color::Yellow)),
            Span::styled("nalyze ", Style::default().fg(Color::DarkGray)),
            Span::styled("[A]", Style::default().fg(Color::Yellow)),
            Span::styled("track ", Style::default().fg(Color::DarkGray)),
            Span::styled("[r]", Style::default().fg(Color::Yellow)),
            Span::styled("ef ", Style::default().fg(Color::DarkGray)),
            Span::styled("[c]", Style::default().fg(Color::Yellow)),
            Span::styled("mp ", Style::default().fg(Color::DarkGray)),
            Span::styled("[x]", Style::default().fg(Color::Yellow)),
            Span::styled("port ", Style::default().fg(Color::DarkGray)),
        ]);
    } else {
        spans.extend([
            Span::styled("[p]", Style::default().fg(Color::Yellow)),
            Span::styled("rofile ", Style::default().fg(Color::DarkGray)),
            Span::styled("[g]", Style::default().fg(Color::Yellow)),
            Span::styled("enscan ", Style::default().fg(Color::DarkGray)),
        ]);
    }

    spans.extend([
        Span::styled("[b]", Style::default().fg(Color::Yellow)),
        Span::styled("mark ", Style::default().fg(Color::DarkGray)),
        Span::styled("[R]", Style::default().fg(Color::Yellow)),
        Span::styled("eport ", Style::default().fg(Color::DarkGray)),
        Span::styled("[Tab]", Style::default().fg(Color::Yellow)),
        Span::styled("view ", Style::default().fg(Color::DarkGray)),
    ]);

    let bar = Paragraph::new(Line::from(spans));
    frame.render_widget(bar, area);
}

// ============================================================================
// Analysis tab
// ============================================================================

fn draw_analysis_tab(frame: &mut Frame, app: &App) {
    let size = frame.area();

    let has_tracking = app.tracking_data.is_some();

    let chunks = if has_tracking {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),           // Header
                Constraint::Percentage(25),      // Spectrum
                Constraint::Length(6),           // Tracking sparkline
                Constraint::Min(6),              // Measurement readout
                Constraint::Length(2),           // Keybindings
            ])
            .split(size)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),           // Header
                Constraint::Percentage(30),      // Spectrum
                Constraint::Length(0),           // No tracking
                Constraint::Min(6),              // Measurement readout
                Constraint::Length(2),           // Keybindings
            ])
            .split(size)
    };

    draw_header(frame, app, chunks[0]);
    draw_spectrum(frame, app, &app.dsp, chunks[1]);
    if has_tracking {
        draw_tracking_sparkline(frame, app, chunks[2]);
    }
    draw_measurement_readout(frame, app, chunks[3]);
    draw_keybindings(frame, app, chunks[4]);
}

/// SNR tracking sparkline over time.
fn draw_tracking_sparkline(frame: &mut Frame, app: &App, area: Rect) {
    let tracking_label = if app.tracking_active { "TRACKING" } else { "PAUSED" };
    let label_color = if app.tracking_active { Color::Green } else { Color::DarkGray };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(vec![
            Span::styled(" SNR ", Style::default().fg(Color::Cyan)),
            Span::styled(tracking_label, Style::default().fg(label_color)),
            Span::raw(" "),
        ]);

    if let Some(ref snap) = app.tracking_data {
        // Convert SNR (timestamp, value) pairs to u64 for Sparkline
        let snr_data: Vec<u64> = snap.snr.iter().map(|&(_t, v)| {
            // Shift range: SNR typically -10 to +40 dB → 0..50
            (v + 10.0).clamp(0.0, 60.0) as u64
        }).collect();

        if snr_data.is_empty() {
            let para = Paragraph::new(Line::from(Span::styled(
                "  Collecting data...",
                Style::default().fg(Color::DarkGray),
            ))).block(block);
            frame.render_widget(para, area);
        } else {
            let sparkline = Sparkline::default()
                .block(block)
                .data(&snr_data)
                .max(60)
                .style(Style::default().fg(Color::Cyan));
            frame.render_widget(sparkline, area);
        }
    } else {
        let para = Paragraph::new(Line::from(Span::styled(
            "  Press [A] to start tracking",
            Style::default().fg(Color::DarkGray),
        ))).block(block);
        frame.render_widget(para, area);
    }
}

/// Measurement results and analysis readout panel.
fn draw_measurement_readout(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ])
        .split(area);

    // Left: measurement/analysis results
    draw_analysis_results(frame, app, chunks[0]);

    // Right: tracking summary + reference status
    draw_tracking_summary(frame, app, chunks[1]);
}

/// Left panel: latest analysis result.
fn draw_analysis_results(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Analysis Results ");

    let lines = match &app.analysis_result {
        None => {
            vec![
                Line::from(Span::styled(
                    "  No analysis run yet",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    "  Press [a] to measure signal",
                    Style::default().fg(Color::DarkGray),
                )),
            ]
        }
        Some(result) => format_analysis_result(result),
    };

    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, area);
}

/// Format an AnalysisResult into display lines.
fn format_analysis_result(result: &wavecore::analysis::AnalysisResult) -> Vec<Line<'static>> {
    use wavecore::analysis::AnalysisResult;
    match result {
        AnalysisResult::Measurement(r) => vec![
            Line::from(vec![
                Span::styled("  -3dB BW: ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.1} kHz", r.bandwidth_3db_hz / 1e3), Style::default().fg(Color::Yellow)),
            ]),
            Line::from(vec![
                Span::styled("  -6dB BW: ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.1} kHz", r.bandwidth_6db_hz / 1e3), Style::default().fg(Color::Yellow)),
            ]),
            Line::from(vec![
                Span::styled("  OccupBW: ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.1} kHz ({:.1}%)", r.occupied_bw_hz / 1e3, r.obw_percent), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("  Ch Pwr:  ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.1} dBFS", r.channel_power_dbfs), color_by_level(r.channel_power_dbfs)),
            ]),
            Line::from(vec![
                Span::styled("  ACPR:    ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("L:{:.1} U:{:.1} dBc", r.acpr_lower_dbc, r.acpr_upper_dbc), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("  PAPR:    ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.1} dB", r.papr_db), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("  Offset:  ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.1} Hz", r.freq_offset_hz), Style::default().fg(Color::White)),
            ]),
        ],
        AnalysisResult::Burst(r) => vec![
            Line::from(vec![
                Span::styled("  Bursts:  ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{}", r.burst_count), Style::default().fg(Color::Yellow)),
            ]),
            Line::from(vec![
                Span::styled("  Width:   ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.1} us", r.mean_pulse_width_us), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("  PRI:     ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.1} us", r.mean_pri_us), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("  Duty:    ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.1}%", r.duty_cycle * 100.0), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("  SNR:     ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.1} dB", r.mean_burst_snr_db), color_by_snr(r.mean_burst_snr_db)),
            ]),
        ],
        AnalysisResult::Modulation(r) => vec![
            Line::from(vec![
                Span::styled("  Type:    ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{}", r.modulation_type), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled("  Conf:    ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.0}%", r.confidence * 100.0), Style::default().fg(Color::White)),
            ]),
            if let Some(rate) = r.symbol_rate_hz {
                Line::from(vec![
                    Span::styled("  SymRate: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{:.1} baud", rate), Style::default().fg(Color::Yellow)),
                ])
            } else {
                Line::from(Span::raw(""))
            },
            if let Some(dev) = r.fm_deviation_hz {
                Line::from(vec![
                    Span::styled("  FM Dev:  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{:.1} kHz", dev / 1e3), Style::default().fg(Color::Yellow)),
                ])
            } else {
                Line::from(Span::raw(""))
            },
        ],
        AnalysisResult::Comparison(r) => vec![
            Line::from(vec![
                Span::styled("  RMS diff:", Style::default().fg(Color::DarkGray)),
                Span::styled(format!(" {:.2} dB", r.rms_diff_db), Style::default().fg(Color::Yellow)),
            ]),
            Line::from(vec![
                Span::styled("  Peak:    ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.2} dB (bin {})", r.peak_diff_db, r.peak_diff_bin), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("  Corr:    ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.4}", r.correlation), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("  New sig: ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{}", r.new_signals.len()), Style::default().fg(Color::Green)),
                Span::styled("  Lost: ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{}", r.lost_signals.len()), Style::default().fg(Color::Red)),
            ]),
        ],
        AnalysisResult::Tracking(snap) => vec![
            Line::from(vec![
                Span::styled("  Duration:", Style::default().fg(Color::DarkGray)),
                Span::styled(format!(" {:.1}s", snap.summary.duration_secs), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("  SNR avg: ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.1} dB", snap.summary.snr_mean), color_by_snr(snap.summary.snr_mean)),
            ]),
            Line::from(vec![
                Span::styled("  Drift:   ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.2} Hz/s", snap.summary.freq_drift_hz_per_sec), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("  Stabil:  ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.0}%", snap.summary.stability_score * 100.0), Style::default().fg(Color::White)),
            ]),
        ],
        AnalysisResult::ExportComplete { path, format } => vec![
            Line::from(vec![
                Span::styled("  Exported: ", Style::default().fg(Color::Green)),
                Span::styled(path.clone(), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("  Format:   ", Style::default().fg(Color::DarkGray)),
                Span::styled(format.clone(), Style::default().fg(Color::White)),
            ]),
        ],
        AnalysisResult::Bitstream(r) => vec![
            Line::from(vec![
                Span::styled("  Length:  ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{} bits", r.length), Style::default().fg(Color::Yellow)),
            ]),
            Line::from(vec![
                Span::styled("  Entropy: ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.2} bits/byte", r.entropy_per_byte), Style::default().fg(Color::White)),
            ]),
            if let Some(ref enc) = r.encoding_guess {
                Line::from(vec![
                    Span::styled("  Encode:  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(enc.clone(), Style::default().fg(Color::Cyan)),
                ])
            } else {
                Line::from(Span::raw(""))
            },
        ],
    }
}

/// Right panel: tracking summary and reference status.
fn draw_tracking_summary(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Tracking & Reference ");

    let mut lines = vec![];

    // Tracking status
    let track_status = if app.tracking_active { "ACTIVE" } else { "OFF" };
    let track_color = if app.tracking_active { Color::Green } else { Color::DarkGray };
    lines.push(Line::from(vec![
        Span::styled("  Track: ", Style::default().fg(Color::DarkGray)),
        Span::styled(track_status, Style::default().fg(track_color).add_modifier(Modifier::BOLD)),
    ]));

    // Reference status
    let ref_status = if app.reference_captured { "CAPTURED" } else { "NONE" };
    let ref_color = if app.reference_captured { Color::Cyan } else { Color::DarkGray };
    lines.push(Line::from(vec![
        Span::styled("  Ref:   ", Style::default().fg(Color::DarkGray)),
        Span::styled(ref_status, Style::default().fg(ref_color)),
    ]));

    lines.push(Line::from(Span::raw("")));

    // Tracking summary if available
    if let Some(ref snap) = app.tracking_data {
        let s = &snap.summary;
        lines.push(Line::from(vec![
            Span::styled("  Dur:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{:.1}s", s.duration_secs), Style::default().fg(Color::White)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  SNR:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.1} [{:.1}..{:.1}] dB", s.snr_mean, s.snr_min, s.snr_max),
                color_by_snr(s.snr_mean),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  Power: ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{:.1} dBFS", s.power_mean), color_by_level(s.power_mean)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  Drift: ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{:.2} Hz/s", s.freq_drift_hz_per_sec), Style::default().fg(Color::White)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  Stab:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{:.0}%", s.stability_score * 100.0), Style::default().fg(Color::White)),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            "  No tracking data",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, area);
}

// ============================================================================
// Color helpers
// ============================================================================

fn color_by_level(db: f32) -> Style {
    if db > -20.0 {
        Style::default().fg(Color::Green)
    } else if db > -40.0 {
        Style::default().fg(Color::Yellow)
    } else if db > -60.0 {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn color_by_snr(snr: f32) -> Style {
    if snr > 20.0 {
        Style::default().fg(Color::Green)
    } else if snr > 10.0 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Red)
    }
}

fn color_by_cpu(load: f32) -> Style {
    if load < 30.0 {
        Style::default().fg(Color::Green)
    } else if load < 70.0 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    }
}

fn decoder_color(decoder: &str) -> Style {
    match decoder {
        "pocsag" => Style::default().fg(Color::LightCyan).add_modifier(Modifier::BOLD),
        "adsb" => Style::default().fg(Color::LightGreen).add_modifier(Modifier::BOLD),
        "rds" => Style::default().fg(Color::LightMagenta).add_modifier(Modifier::BOLD),
        _ => Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
    }
}
