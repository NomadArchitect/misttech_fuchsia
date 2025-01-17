// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::codec::CodecInterface;
use crate::configurator::Configurator;
use crate::dai::DaiInterface;
use anyhow::{anyhow, Error};
use fidl_fuchsia_io as fio;
use futures::lock::Mutex;
use futures::TryStreamExt;
use std::path::Path;
use std::sync::Arc;

/// Finds any codec devices and calls the `process_new_codec` Configurator callback.
/// If `break_count` is non-zero then once `break_count` codecs are found return.
/// If `dev_proxy` can't be cloned an error is returned.
pub async fn find_codecs<T: Configurator>(
    dev_proxy: &fio::DirectoryProxy,
    break_count: u32,
    configurator: Arc<Mutex<T>>,
) -> Result<(), Error> {
    let mut watcher = fuchsia_fs::directory::Watcher::new(dev_proxy).await?;

    let mut codecs_found = 0;

    while let Some(msg) = watcher.try_next().await? {
        match msg.event {
            fuchsia_fs::directory::WatchEvent::EXISTING
            | fuchsia_fs::directory::WatchEvent::ADD_FILE => {
                if msg.filename == Path::new(".") {
                    continue;
                }
                let path = Path::new(&msg.filename);
                log::info!("Found new codec in devfs node: {:?}", path);
                let interface = CodecInterface::new(Clone::clone(dev_proxy), &path);
                if let Err(e) = configurator.lock().await.process_new_codec(interface).await {
                    if break_count != 0 {
                        // Error when we want to break on count, then report it and exit.
                        return Err(anyhow!("Codec processing error: {:?}", e));
                    } else {
                        // Otherwise we continue finding codecs.
                        log::warn!("Codec processing error: {:?}", e);
                    }
                }
                codecs_found += 1;
                if codecs_found == break_count {
                    return Ok(());
                }
            }
            _ => continue,
        }
    }
    Ok(())
}

/// Finds any DAI devices and calls the `process_new_dai` Configurator callback.
/// If `break_count` is non-zero then once `break_count` DAIs are found return.
/// If `dev_proxy` can't be cloned an error is returned.
pub async fn find_dais<T: Configurator>(
    dev_proxy: &fio::DirectoryProxy,
    break_count: u32,
    configurator: Arc<Mutex<T>>,
) -> Result<(), Error> {
    let mut watcher = fuchsia_fs::directory::Watcher::new(dev_proxy).await?;

    let mut dais_found = 0;
    let mut stream_config_serve_tasks = Vec::new();

    while let Some(msg) = watcher.try_next().await? {
        match msg.event {
            fuchsia_fs::directory::WatchEvent::EXISTING
            | fuchsia_fs::directory::WatchEvent::ADD_FILE => {
                if msg.filename == Path::new(".") {
                    continue;
                }
                let path = Path::new(&msg.filename);
                log::info!("Found new DAI in devfs node: {:?}", path);
                let interface = DaiInterface::new(Clone::clone(dev_proxy), &path);
                let mut configurator = configurator.lock().await;
                if let Err(e) = configurator.process_new_dai(interface).await {
                    if break_count != 0 {
                        // Error when we want to break on count, then report it and exit.
                        return Err(anyhow!("DAI processing error: {:?}", e));
                    } else {
                        // Otherwise we continue finding DAIs.
                        log::warn!("DAI processing error: {:?}", e);
                    }
                } else {
                    // If we are not breaking then serve the required interfaces.
                    if break_count == 0 {
                        match configurator.serve_interface() {
                            Err(e) => log::warn!("Interface serving error: {:?}", e),
                            Ok(task) => {
                                stream_config_serve_tasks.push(task);
                            }
                        }
                    }
                }
                dais_found += 1;
                if dais_found == break_count {
                    return Ok(());
                }
            }
            _ => continue,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::testing::tests::{get_dev_proxy, NullConfigurator};
    use anyhow::Result;

    #[fuchsia_async::run_singlethreaded(test)]
    #[ignore]
    async fn test_find_codecs() -> Result<()> {
        let (_realm_instance, dev_proxy) = get_dev_proxy("class/codec").await?;
        let config = Config::new()?;
        let configurator = Arc::new(Mutex::new(NullConfigurator::new(config)?));
        find_codecs(&dev_proxy, 2, configurator).await?;
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    #[ignore]
    async fn test_find_dais() -> Result<()> {
        let (_realm_instance, dev_proxy) = get_dev_proxy("class/dai").await?;
        let config = Config::new()?;
        let configurator = Arc::new(Mutex::new(NullConfigurator::new(config)?));
        find_dais(&dev_proxy, 1, configurator).await?;
        Ok(())
    }
}
