use std::fs::File;

use sdag::Result;
use sdag_wallet_base::*;
use serde_json;

const SETTINGS_FILE: &str = "settings.json";

#[derive(Debug, Serialize, Deserialize)]
pub struct Settings {
    pub hub_url: Vec<String>,
    pub mnemonic: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            hub_url: vec![String::from("127.0.0.1:6615")],
            mnemonic: mnemonic("")
                .expect("failed to generate mnemonic")
                .to_string(),
        }
    }
}

impl Settings {
    pub fn show_config(&self) {
        use std::io::stdout;
        println!("settings:");
        serde_json::to_writer_pretty(stdout(), self).unwrap();
        println!("\n");
    }
}

fn open_settings() -> Result<Settings> {
    let mut settings_path = ::std::env::current_dir()?;
    settings_path.push(SETTINGS_FILE);
    let file = File::open(settings_path)?;
    let settings = serde_json::from_reader(file)?;
    Ok(settings)
}

fn save_settings(settings: &Settings) -> Result<()> {
    let mut settings_path = ::std::env::current_dir()?;
    settings_path.push(SETTINGS_FILE);

    let file = File::create(settings_path)?;

    serde_json::to_writer_pretty(file, settings)?;
    Ok(())
}

pub fn update_mnemonic(mnemonic: &str) -> Result<()> {
    let mnemonic = Mnemonic::from(mnemonic)?.to_string();
    let mut settings = get_settings();
    if settings.mnemonic != mnemonic {
        println!("will update mnemonic to: {}", mnemonic);
        settings.mnemonic = mnemonic;
    }
    save_settings(&settings)
}

pub fn get_settings() -> Settings {
    match open_settings() {
        Ok(s) => s,
        Err(_) => {
            warn!("can't open settings.json, will use default settings");
            let settings = Settings::default();
            save_settings(&settings).expect("failed to save settings");
            settings
        }
    }
}
