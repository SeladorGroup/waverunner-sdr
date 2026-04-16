export interface SpectrumFrame {
  spectrum_db: number[];
  noise_floor_db: number;
  rms_dbfs: number;
  snr_db: number;
  spectral_flatness: number;
  signal_stats: SignalStats;
  agc_gain_db: number;
  block_count: number;
}

export interface SignalStats {
  mean: { re: number; im: number };
  variance: number;
  rms: number;
  peak: number;
  crest_factor_db: number;
  skewness: number;
  kurtosis: number;
  excess_kurtosis: number;
}

export interface Detection {
  bin: number;
  power_db: number;
  noise_floor_db: number;
  snr_db: number;
  freq_offset_hz: number;
}

export interface DecodedMessage {
  decoder: string;
  elapsed_ms: number;
  summary: string;
  fields: Record<string, string>;
  raw_bits: number[] | null;
}

export interface DemodVisData {
  constellation: [number, number][];
  pll_phase_error: number;
  pll_frequency_hz: number;
  pll_locked: boolean;
  agc_gain_db: number;
  mode: string;
}

export type HealthStatus = "Normal" | "Warning" | "Critical";

export interface LatencyBreakdown {
  dc_removal_us: number;
  fft_us: number;
  cfar_us: number;
  statistics_us: number;
  decoder_feed_us: number;
  demod_us: number;
  total_us: number;
}

export interface SessionStats {
  blocks_processed: number;
  blocks_dropped: number;
  processing_time_us: number;
  throughput_msps: number;
  cpu_load_percent: number;
  buffer_occupancy: number;
  events_dropped: number;
  health: HealthStatus;
  latency: LatencyBreakdown;
}

export type StatusUpdate =
  | "Streaming"
  | { RecordingStarted: string }
  | { RecordingStopped: number }
  | { DecoderEnabled: string }
  | { DecoderDisabled: string }
  | { FrequencyChanged: number }
  | { GainChanged: GainMode }
  | { LoadShedding: number }
  | { HealthChanged: HealthStatus };

export type GainMode = "Auto" | { Manual: number };

export interface DeviceInfo {
  name: string;
  driver: string;
  serial: string | null;
  index: number;
  frequency_range: [number, number];
  sample_rate_range: [number, number];
  gain_range: [number, number];
  available_gains: number[];
}

export interface SessionConfig {
  schema_version: number;
  device_index: number;
  frequency: number;
  sample_rate: number;
  gain: GainMode;
  ppm: number;
  fft_size: number;
  pfa: number;
}

export interface DemodConfig {
  mode: string;
  audio_rate: number;
  bandwidth: number | null;
  bfo: number | null;
  squelch: number | null;
  deemph_us: number | null;
  output_wav: string | null;
  emit_visualization?: boolean;
  spectrum_update_interval_blocks?: number;
}

// Analysis types

export interface MeasurementReport {
  bandwidth_3db_hz: number;
  bandwidth_6db_hz: number;
  occupied_bw_hz: number;
  obw_percent: number;
  channel_power_dbfs: number;
  acpr_lower_dbc: number;
  acpr_upper_dbc: number;
  papr_db: number;
  freq_offset_hz: number;
}

export interface BurstDescriptor {
  start: number;
  end: number;
  duration_us: number;
  peak_power_db: number;
  mean_power_db: number;
}

export interface BurstReport {
  burst_count: number;
  bursts: BurstDescriptor[];
  mean_pulse_width_us: number;
  pulse_width_std_us: number;
  mean_pri_us: number;
  pri_std_us: number;
  duty_cycle: number;
  mean_burst_snr_db: number;
}

export interface ModulationReport {
  modulation_type: string;
  confidence: number;
  symbol_rate_hz: number | null;
  am_depth: number | null;
  fm_deviation_hz: number | null;
  amplitude_levels: number | null;
  phase_states: number | null;
}

export interface ComparisonReport {
  diff_db: number[];
  rms_diff_db: number;
  peak_diff_db: number;
  peak_diff_bin: number;
  correlation: number;
  new_signals: [number, number][];   // [bin, excess_db]
  lost_signals: [number, number][];  // [bin, deficit_db]
}

export interface TrackingSummary {
  duration_secs: number;
  snr_mean: number;
  snr_min: number;
  snr_max: number;
  power_mean: number;
  freq_drift_hz_per_sec: number;
  stability_score: number;
}

export interface TrackingSnapshot {
  snr: [number, number][];
  power: [number, number][];
  noise_floor: [number, number][];
  freq_offset: [number, number][];
  summary: TrackingSummary;
}

export interface BitstreamReport {
  length: usize;
  ones_fraction: number;
  max_run_length: number;
  entropy_per_byte: number;
  encoding_guess: string | null;
  frame_lengths: number[];
  hex_dump: string;
}

type usize = number;

export type AnalysisResult =
  | { Measurement: MeasurementReport }
  | { Burst: BurstReport }
  | { Modulation: ModulationReport }
  | { Comparison: ComparisonReport }
  | { Bitstream: BitstreamReport }
  | { Tracking: TrackingSnapshot }
  | { ExportComplete: { path: string; format: string } };

export interface AnalysisEvent {
  id: number;
  result: AnalysisResult;
}

// Timeline & Annotation types

export type AnnotationKind = "Bookmark" | "Note" | "Tag";

export interface Annotation {
  id: number;
  timestamp_s: number;
  kind: AnnotationKind;
  text: string;
  frequency_hz: number;
}

export type TimelineEntry =
  | { FreqChange: { timestamp_s: number; freq_hz: number } }
  | { GainChange: { timestamp_s: number; gain: string } }
  | { RecordStart: { timestamp_s: number; path: string } }
  | { RecordStop: { timestamp_s: number; samples: number } }
  | { DecoderEnabled: { timestamp_s: number; name: string } }
  | { DecoderDisabled: { timestamp_s: number; name: string } }
  | { Annotation: { timestamp_s: number; id: number } }
  | { LoadShedding: { timestamp_s: number; level: number } };

// Report types

export interface ScanDetection {
  frequency_hz: number;
  power_db: number;
  snr_db: number;
  bandwidth_hz: number;
}

export interface ScanReport {
  start_freq: number;
  end_freq: number;
  step_hz: number;
  dwell_ms: number;
  signals_found: number;
  detections: ScanDetection[];
}

export interface SessionMetadata {
  start_time: string;
  duration_secs: number;
  center_freq: number;
  sample_rate: number;
  gain: string;
  fft_size: number;
}

export interface SessionReport {
  metadata: SessionMetadata;
  scan_results: ScanReport | null;
  annotations: Annotation[];
}
