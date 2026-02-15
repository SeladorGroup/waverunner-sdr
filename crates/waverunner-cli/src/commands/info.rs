use anyhow::{Context, Result};

use wavecore::hardware::DeviceEnumerator;
use wavecore::hardware::rtlsdr::RtlSdrDevice;

pub async fn run(device_index: u32) -> Result<()> {
    println!("Waverunner SDR Platform");
    println!("=======================\n");

    // Enumerate devices
    let devices = RtlSdrDevice::enumerate().context("Failed to enumerate devices")?;

    if devices.is_empty() {
        println!("No SDR devices found.");
        println!("\nMake sure your RTL-SDR is plugged in and drivers are installed.");
        println!("On Linux: sudo apt install librtlsdr-dev  or  pacman -S rtl-sdr");
        return Ok(());
    }

    println!("Found {} device(s):\n", devices.len());

    for dev_info in &devices {
        println!("  [{}] {}", dev_info.index, dev_info.name);
        println!("      Driver: {}", dev_info.driver);
        if let Some(serial) = &dev_info.serial {
            println!("      Serial: {serial}");
        }
    }

    // Open the selected device for detailed info
    println!("\nOpening device #{device_index}...\n");
    let device = RtlSdrDevice::open(device_index)
        .context(format!("Failed to open device #{device_index}"))?;

    let info = device.info().context("Failed to get device info")?;

    println!("Device Details:");
    println!("  Name:         {}", info.name);
    println!(
        "  Frequency:    {:.3} MHz - {:.3} MHz",
        info.frequency_range.0 / 1e6,
        info.frequency_range.1 / 1e6,
    );
    println!(
        "  Sample Rate:  {:.3} kS/s - {:.3} MS/s",
        info.sample_rate_range.0 / 1e3,
        info.sample_rate_range.1 / 1e6,
    );
    println!(
        "  Gain Range:   {:.1} - {:.1} dB",
        info.gain_range.0, info.gain_range.1,
    );

    if !info.available_gains.is_empty() {
        let gains: Vec<String> = info
            .available_gains
            .iter()
            .map(|g| format!("{g:.1}"))
            .collect();
        println!("  Gain Steps:   {} dB", gains.join(", "));
    }

    Ok(())
}
