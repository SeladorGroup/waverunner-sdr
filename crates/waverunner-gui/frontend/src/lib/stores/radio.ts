import { get, writable } from "svelte/store";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import type {
  SpectrumFrame,
  SignalStats,
  Detection,
  DecodedMessage,
  DemodVisData,
  SessionStats,
  StatusUpdate,
  GainMode,
  SessionConfig,
  DemodConfig,
  AnalysisResult,
  AnalysisEvent,
  TrackingSnapshot,
  Bookmark,
  CaptureRecord,
  DecoderInfo,
} from "../types";

// Connection state
export const connected = writable(false);
export const frequency = writable(100e6);
export const sampleRate = writable(2.048e6);
export const gain = writable<GainMode>("Auto");
export const demodMode = writable("OFF");
export const enabledDecoders = writable<string[]>([]);
export const recordingActive = writable(false);
export const recordingPath = writable<string | null>(null);

// DSP state
export const spectrumData = writable<number[]>([]);
export const peakHold = writable<number[]>([]);
export const noiseFloorDb = writable(-100);
export const rmsDbfs = writable(-100);
export const snrDb = writable(0);
export const signalStats = writable<SignalStats>({
  mean: { re: 0, im: 0 },
  variance: 0,
  rms: 0,
  peak: 0,
  crest_factor_db: 0,
  skewness: 0,
  kurtosis: 0,
  excess_kurtosis: 0,
});

// Waterfall
const MAX_WATERFALL_ROWS = 200;
export const waterfallRows = writable<number[][]>([]);

// Detections
export const detections = writable<Detection[]>([]);

// Session stats
export const sessionStats = writable<SessionStats>({
  blocks_processed: 0,
  blocks_dropped: 0,
  processing_time_us: 0,
  throughput_msps: 0,
  cpu_load_percent: 0,
  buffer_occupancy: 0,
  events_dropped: 0,
  health: "Normal",
  latency: {
    dc_removal_us: 0,
    fft_us: 0,
    cfar_us: 0,
    statistics_us: 0,
    decoder_feed_us: 0,
    demod_us: 0,
    total_us: 0,
  },
});

// Decoded messages
const MAX_DECODED = 50;
export const decodedMessages = writable<DecodedMessage[]>([]);

// Demod visualization
export const constellation = writable<[number, number][]>([]);
export const pllLocked = writable(false);
export const pllFrequencyHz = writable(0);
export const pllPhaseError = writable(0);
export const agcGainDb = writable(0);

// Status
export const statusMessage = writable("");
export const errorMessage = writable("");

// Mode
export const activeMode = writable("");
export const modeStatus = writable("");

// Analysis
export const analysisResult = writable<AnalysisResult | null>(null);
export const trackingData = writable<TrackingSnapshot | null>(null);
export const trackingActive = writable(false);
export const referenceCapture = writable(false);

// Spectral flatness (from SpectrumFrame, separate for SignalStats display)
export const spectralFlatness = writable(0);

// Display settings (shared with renderers)
export const displayDbMin = writable(-120);
export const displayDbMax = writable(0);
export const showPeakHold = writable(true);

// Peak hold state
let peakHoldData: number[] = [];

function resetSessionState(): void {
  connected.set(false);
  demodMode.set("OFF");
  enabledDecoders.set([]);
  recordingActive.set(false);
  recordingPath.set(null);
  spectrumData.set([]);
  waterfallRows.set([]);
  peakHoldData = [];
  peakHold.set([]);
  detections.set([]);
  decodedMessages.set([]);
  constellation.set([]);
  pllLocked.set(false);
  pllFrequencyHz.set(0);
  pllPhaseError.set(0);
  agcGainDb.set(0);
  statusMessage.set("");
  errorMessage.set("");
  activeMode.set("");
  modeStatus.set("");
  analysisResult.set(null);
  trackingData.set(null);
  trackingActive.set(false);
  referenceCapture.set(false);
  sessionStats.set({
    blocks_processed: 0,
    blocks_dropped: 0,
    processing_time_us: 0,
    throughput_msps: 0,
    cpu_load_percent: 0,
    buffer_occupancy: 0,
    events_dropped: 0,
    health: "Normal",
    latency: {
      dc_removal_us: 0,
      fft_us: 0,
      cfar_us: 0,
      statistics_us: 0,
      decoder_feed_us: 0,
      demod_us: 0,
      total_us: 0,
    },
  });
}

export function setupEventListeners(): void {
  listen<SpectrumFrame>("wr:spectrum", (event) => {
    const frame = event.payload;
    spectrumData.set(frame.spectrum_db);
    noiseFloorDb.set(frame.noise_floor_db);
    rmsDbfs.set(frame.rms_dbfs);
    snrDb.set(frame.snr_db);
    signalStats.set(frame.signal_stats);
    agcGainDb.set(frame.agc_gain_db);
    spectralFlatness.set(frame.spectral_flatness);

    // Peak hold with decay
    if (peakHoldData.length !== frame.spectrum_db.length) {
      peakHoldData = [...frame.spectrum_db];
    } else {
      for (let i = 0; i < peakHoldData.length; i++) {
        if (frame.spectrum_db[i] > peakHoldData[i]) {
          peakHoldData[i] = frame.spectrum_db[i];
        } else {
          peakHoldData[i] = Math.max(
            peakHoldData[i] - 0.5,
            frame.spectrum_db[i],
          );
        }
      }
    }
    peakHold.set([...peakHoldData]);

    // Push to waterfall
    waterfallRows.update((rows) => {
      const next = [...rows, frame.spectrum_db];
      if (next.length > MAX_WATERFALL_ROWS) {
        next.splice(0, next.length - MAX_WATERFALL_ROWS);
      }
      return next;
    });
  });

  listen<Detection[]>("wr:detections", (event) => {
    detections.set(event.payload);
  });

  listen<SessionStats>("wr:stats", (event) => {
    sessionStats.set(event.payload);
  });

  listen<DecodedMessage>("wr:decoded", (event) => {
    decodedMessages.update((msgs) => {
      const next = [...msgs, event.payload];
      if (next.length > MAX_DECODED) {
        next.splice(0, next.length - MAX_DECODED);
      }
      return next;
    });
  });

  listen<DemodVisData>("wr:demod-vis", (event) => {
    const vis = event.payload;
    constellation.set(vis.constellation);
    pllLocked.set(vis.pll_locked);
    pllFrequencyHz.set(vis.pll_frequency_hz);
    pllPhaseError.set(vis.pll_phase_error);
    agcGainDb.set(vis.agc_gain_db);
    demodMode.set(vis.mode);
  });

  listen<StatusUpdate>("wr:status", (event) => {
    const status = event.payload;
    if (status === "Streaming") {
      statusMessage.set("Streaming");
      connected.set(true);
    } else if (status === "TrackingStarted") {
      trackingActive.set(true);
      statusMessage.set("Tracking started");
    } else if (status === "TrackingStopped") {
      trackingActive.set(false);
      statusMessage.set("Tracking stopped");
    } else if (status === "AnalysisReferenceCapture") {
      referenceCapture.set(true);
      statusMessage.set("Reference capture saved");
    } else if (typeof status === "object") {
      if ("FrequencyChanged" in status) {
        frequency.set(status.FrequencyChanged);
      } else if ("GainChanged" in status) {
        gain.set(status.GainChanged);
      } else if ("DecoderEnabled" in status) {
        enabledDecoders.update((names) =>
          names.includes(status.DecoderEnabled)
            ? names
            : [...names, status.DecoderEnabled],
        );
        statusMessage.set(`Decoder: ${status.DecoderEnabled}`);
      } else if ("DecoderDisabled" in status) {
        enabledDecoders.update((names) =>
          names.filter((name) => name !== status.DecoderDisabled),
        );
        statusMessage.set(`Decoder off`);
      } else if ("RecordingStarted" in status) {
        recordingActive.set(true);
        recordingPath.set(status.RecordingStarted);
        statusMessage.set(`Recording...`);
      } else if ("RecordingStopped" in status) {
        recordingActive.set(false);
        recordingPath.set(null);
        statusMessage.set(`Recorded ${status.RecordingStopped} samples`);
      } else if ("TimelineExported" in status) {
        statusMessage.set(`Timeline saved: ${status.TimelineExported}`);
      } else if ("ModeChanged" in status) {
        activeMode.set(status.ModeChanged.mode);
        modeStatus.set(status.ModeChanged.state);
      } else if ("LoadShedding" in status) {
        statusMessage.set(
          status.LoadShedding > 0
            ? `Load shedding: level ${status.LoadShedding}`
            : "Load shedding cleared",
        );
      } else if ("HealthChanged" in status) {
        statusMessage.set(`Pipeline health: ${status.HealthChanged}`);
      }
    }
  });

  listen<string>("wr:error", (event) => {
    errorMessage.set(event.payload);
    setTimeout(() => errorMessage.set(""), 5000);
  });

  listen<string>("wr:mode-status", (event) => {
    modeStatus.set(event.payload);
  });

  listen<AnalysisEvent>("wr:analysis-result", (event) => {
    analysisResult.set(event.payload.result);
  });

  listen<TrackingSnapshot>("wr:tracking", (event) => {
    trackingData.set(event.payload);
  });
}

// Command wrappers
export async function connectDevice(config: SessionConfig): Promise<void> {
  if (get(connected)) {
    await disconnectDevice();
  }
  await invoke("connect_device", { config });
  connected.set(true);
  frequency.set(config.frequency);
  sampleRate.set(config.sample_rate);
  gain.set(config.gain);
}

export async function disconnectDevice(): Promise<void> {
  await invoke("disconnect_device");
  resetSessionState();
}

export async function replayFile(
  path: string,
  sample_rate: number,
  freq: number,
): Promise<void> {
  if (get(connected)) {
    await disconnectDevice();
  }
  await invoke("replay_file", {
    path,
    sampleRate: sample_rate,
    frequency: freq,
  });
  connected.set(true);
  frequency.set(freq);
  sampleRate.set(sample_rate);
}

export async function cmdTune(freq: number): Promise<void> {
  await invoke("tune", { frequency: freq });
}

export async function cmdSetGain(mode: GainMode): Promise<void> {
  await invoke("set_gain", { mode });
  gain.set(mode);
}

export async function cmdSetSampleRate(rate: number): Promise<void> {
  await invoke("set_sample_rate", { rate });
  sampleRate.set(rate);
}

export async function cmdStartDemod(config: DemodConfig): Promise<void> {
  await invoke("start_demod", { config });
}

export async function cmdStopDemod(): Promise<void> {
  await invoke("stop_demod");
}

export async function cmdEnableDecoder(name: string): Promise<void> {
  await invoke("enable_decoder", { name });
}

export async function cmdDisableDecoder(name: string): Promise<void> {
  await invoke("disable_decoder", { name });
}

export async function cmdStartRecord(
  path: string,
  format: string,
): Promise<void> {
  await invoke("start_record", { path, format });
}

export async function cmdStopRecord(): Promise<void> {
  await invoke("stop_record");
}

export async function generateCapturePath(
  format: string,
  label?: string,
): Promise<string> {
  return await invoke("generate_capture_path", { format, label });
}

export async function listRecentCaptures(limit?: number): Promise<CaptureRecord[]> {
  return await invoke("list_recent_captures", { limit });
}

export async function listBookmarks(): Promise<Bookmark[]> {
  return await invoke("list_bookmarks");
}

export async function saveCurrentBookmark(
  name: string,
  mode?: string | null,
  decoder?: string | null,
  notes?: string | null,
): Promise<void> {
  await invoke("save_current_bookmark", { name, mode, decoder, notes });
}

export async function removeBookmark(name: string): Promise<void> {
  await invoke("remove_bookmark", { name });
}

export async function getAvailableDevices(): Promise<unknown[]> {
  return await invoke("get_available_devices");
}

export async function getAvailableDecoders(): Promise<string[]> {
  return await invoke("get_available_decoders");
}

export async function getDecoderCatalog(): Promise<DecoderInfo[]> {
  return await invoke("get_decoder_catalog");
}

// Mode commands
export interface ProfileInfo {
  name: string;
  description: string;
}

export async function listProfiles(): Promise<ProfileInfo[]> {
  return await invoke("list_profiles");
}

export async function activateProfile(name: string): Promise<void> {
  await invoke("activate_profile", { name });
  activeMode.set(name);
}

export async function activateGeneralScan(
  scanStart: number,
  scanEnd: number,
): Promise<void> {
  await invoke("activate_general_scan", { scanStart, scanEnd });
  activeMode.set("general");
}

export async function deactivateMode(): Promise<void> {
  await invoke("deactivate_mode");
  activeMode.set("");
  modeStatus.set("");
}

// Analysis commands
export async function measureSignal(): Promise<void> {
  await invoke("measure_signal");
}

export async function analyzeBurst(thresholdDb: number = 10): Promise<void> {
  await invoke("analyze_burst", { thresholdDb });
}

export async function estimateModulation(): Promise<void> {
  await invoke("estimate_modulation");
}

export async function compareSpectra(): Promise<void> {
  await invoke("compare_spectra");
}

export async function captureReference(): Promise<void> {
  await invoke("capture_reference");
  referenceCapture.set(true);
}

export async function toggleTracking(): Promise<void> {
  const active = await invoke<boolean>("toggle_tracking");
  trackingActive.set(active);
}

export async function exportData(format: string, path: string): Promise<void> {
  await invoke("export_data", { format, path });
}
