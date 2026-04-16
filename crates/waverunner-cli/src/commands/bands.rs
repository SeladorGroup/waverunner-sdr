use anyhow::Result;
use wavecore::frequency_db::{FrequencyDb, Region, ServiceType};

#[derive(clap::Args)]
pub struct BandsArgs {
    /// Region to display (auto-detected if omitted)
    #[arg(short, long)]
    pub region: Option<String>,

    /// Filter by service type (e.g. aviation, maritime, ism)
    #[arg(short, long)]
    pub service: Option<String>,
}

pub fn run(args: BandsArgs) -> Result<()> {
    let db = if let Some(ref r) = args.region {
        let region: Region = r.parse().map_err(|e| anyhow::anyhow!("{e}"))?;
        FrequencyDb::new(region)
    } else {
        FrequencyDb::auto_detect()
    };

    let service_filter: Option<ServiceType> = if let Some(ref s) = args.service {
        Some(s.parse().map_err(|e| anyhow::anyhow!("{e}"))?)
    } else {
        None
    };

    println!(
        "Frequency allocations for {} ({})\n",
        db.region,
        db.region.label()
    );

    // Band allocations
    let bands = if let Some(svc) = service_filter {
        db.bands_by_service(svc)
    } else {
        db.bands_for_region()
    };

    if !bands.is_empty() {
        println!(
            "{:<24} {:>14} {:>14}  {:<8} {:<10}",
            "Band", "Start", "End", "Mode", "Decoder"
        );
        println!("{}", "-".repeat(76));
        for band in &bands {
            let start = wavecore::util::format_freq(band.start_hz);
            let end = wavecore::util::format_freq(band.end_hz);
            let decoder = band.decoder.unwrap_or("-");
            println!(
                "{:<24} {:>14} {:>14}  {:<8} {:<10}",
                band.label, start, end, band.modulation, decoder
            );
        }
    }

    // Known frequencies
    let freqs = db.known_frequencies();
    let freqs: Vec<_> = if let Some(svc) = service_filter {
        freqs.into_iter().filter(|f| f.service == svc).collect()
    } else {
        freqs
    };

    if !freqs.is_empty() {
        println!(
            "\n{:<24} {:>14}  {:<8} {:<10}",
            "Channel", "Frequency", "Mode", "Decoder"
        );
        println!("{}", "-".repeat(62));
        for freq in &freqs {
            let f = wavecore::util::format_freq(freq.freq_hz);
            let decoder = freq.decoder.unwrap_or("-");
            println!(
                "{:<24} {:>14}  {:<8} {:<10}",
                freq.label, f, freq.modulation, decoder
            );
        }
    }

    println!("\n{} bands, {} known frequencies", bands.len(), freqs.len());
    Ok(())
}
