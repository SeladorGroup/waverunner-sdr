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
use ratatui::widgets::{Block, Borders, Paragraph};

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

    let mut spans = vec![
        Span::styled(" WaveRunner ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(&freq_str, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(&rate_str, Style::default().fg(Color::Green)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(mode_str, Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("Step: {step_str}"), Style::default().fg(Color::Blue)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("Gain: {gain_str}"), Style::default().fg(Color::White)),
    ];

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

    // Tab indicator
    spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
    spans.push(Span::styled(
        format!("[{}]", app.view_tab.label()),
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
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

/// Keybinding help bar.
fn draw_keybindings(frame: &mut Frame, _app: &App, area: Rect) {
    let spans = vec![
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
        Span::styled("[Tab]", Style::default().fg(Color::Yellow)),
        Span::styled("view ", Style::default().fg(Color::DarkGray)),
        Span::styled("[↑↓]", Style::default().fg(Color::Yellow)),
        Span::styled("tune ", Style::default().fg(Color::DarkGray)),
        Span::styled("[←→]", Style::default().fg(Color::Yellow)),
        Span::styled("step", Style::default().fg(Color::DarkGray)),
    ];

    let bar = Paragraph::new(Line::from(spans));
    frame.render_widget(bar, area);
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
