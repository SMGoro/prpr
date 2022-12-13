use anyhow::{Context, Result};
use macroquad::prelude::*;
use prpr::{build_conf, fs, Prpr};

#[macroquad::main(build_conf)]
async fn main() -> Result<()> {
    set_pc_assets_folder("assets");

    #[cfg(target_arch = "wasm32")]
    let (fs, config) = {
        fn js_err(err: wasm_bindgen::JsValue) -> anyhow::Error {
            anyhow::Error::msg(format!("{err:?}"))
        }
        let params = web_sys::UrlSearchParams::new_with_str(
            &web_sys::window()
                .unwrap()
                .location()
                .search()
                .map_err(js_err)?,
        )
        .map_err(js_err)?;
        let name = params.get("chart").unwrap_or_else(|| "nc".to_string());
        (fs::fs_from_assets(&name)?, None)
    };
    #[cfg(any(target_os = "android", target_os = "ios"))]
    let (fs, config) = (fs::fs_from_assets("moment")?, None);
    #[cfg(all(
        not(target_arch = "wasm32"),
        not(target_os = "android"),
        not(target_os = "ios")
    ))]
    let (fs, config) = {
        let mut args = std::env::args();
        let program = args.next().unwrap();
        let Some(path) = args.next() else {
            anyhow::bail!("Usage: {program} <chart>");
        };
        let mut config = None;
        if let Some(config_path) = args.next() {
            config = Some(serde_yaml::from_str(
                &std::fs::read_to_string(config_path).context("Cannot read from config file")?,
            )?);
        }
        (fs::fs_from_file(&path)?, config)
    };

    let (info, fs) = fs::load_info(fs).await?;
    let config = config.unwrap_or_default();

    let mut fps_time = -1;

    let mut prpr = Prpr::new(info, config, fs, None).await?;
    'app: loop {
        let frame_start = prpr.get_time();
        prpr.update(None)?;
        prpr.render(None)?;
        prpr.ui(true)?;
        prpr.process_keys()?;
        if prpr.should_exit {
            break 'app;
        }

        let t = prpr.get_time();
        let fps_now = t as i32;
        if fps_now != fps_time {
            fps_time = fps_now;
            info!("| {}", (1. / (t - frame_start)) as u32);
        }

        next_frame().await;
    }
    Ok(())
}