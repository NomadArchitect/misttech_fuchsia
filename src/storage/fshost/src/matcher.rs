// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::constants::{
    BLOBFS_PARTITION_LABEL, BOOTPART_DRIVER_PATH, DATA_PARTITION_LABEL, FTL_PARTITION_LABEL,
    FUCHSIA_FVM_PARTITION_LABEL, FVM_DRIVER_PATH, FVM_PARTITION_LABEL, GPT_DRIVER_PATH,
    LEGACY_DATA_PARTITION_LABEL, MBR_DRIVER_PATH, NAND_BROKER_DRIVER_PATH, SUPER_PARTITION_LABEL,
};
use crate::device::{Device, DeviceTag};
use crate::environment::Environment;
use anyhow::{bail, Context, Error};
use async_trait::async_trait;
use fidl_fuchsia_hardware_block::Flag as BlockFlag;
use fs_management::format::DiskFormat;
use std::collections::HashSet;
use std::sync::LazyLock;

#[async_trait]
pub trait Matcher: Send {
    /// Tries to match this device against this matcher. Matching should be infallible.
    async fn match_device(&self, device: &mut dyn Device) -> bool;

    /// Process this device as the format this matcher is for. This is called when this matcher
    /// returns true during matching. This step is fallible - if a device matched a matcher, but
    /// then this step fails, we stop matching and bubble up the error. The matcher may return a
    /// `DeviceTag` which will be used to register the device with the environment.
    async fn process_device(
        &mut self,
        device: &mut dyn Device,
        env: &mut dyn Environment,
    ) -> Result<Option<DeviceTag>, Error>;
}

pub struct Matchers {
    matchers: Vec<Box<dyn Matcher>>,
}

impl Matchers {
    /// Create a new set of matchers. This essentially describes the expected partition layout for
    /// a device.
    pub fn new(config: &fshost_config::Config) -> Self {
        let mut matchers = Vec::<Box<dyn Matcher>>::new();

        // NB: Order is important here!
        // Generally speaking, we want to have more specific matchers first, and more general
        // matchers later.  For example, the GptMatcher needs to come after most others because it
        // will bind to *any* non-removable device, but will only bind once.  It will in turn
        // publish more devices, which will be matched by our other matchers.
        if config.bootpart {
            matchers.push(Box::new(BootpartMatcher::new()));
        }
        if config.nand {
            matchers.push(Box::new(NandMatcher::new()));
        }
        if config.fxfs_blob {
            if !config.netboot {
                matchers.push(Box::new(FxblobMatcher::new(config.ramdisk_image)));
            }
            if config.ramdisk_image || config.netboot {
                matchers.push(Box::new(FxblobOnRecoveryMatcher::new()));
            }
        } else {
            let fvm_matcher = if config.storage_host {
                Box::new(FvmComponentMatcher::new(config.ramdisk_image)) as Box<dyn Matcher>
            } else {
                Box::new(FvmMatcher::new(
                    config.ramdisk_image,
                    config.netboot,
                    config.blobfs,
                    config.data,
                )) as Box<dyn Matcher>
            };
            if config.fvm || (!config.netboot && (config.blobfs || config.data)) {
                matchers.push(fvm_matcher);
            }
        }
        if config.fvm && config.ramdisk_image && !config.storage_host {
            // Add another matcher for the non-ramdisk version of fvm.
            matchers.push(Box::new(PartitionMapMatcher::new(
                DiskFormat::Fvm,
                FVM_DRIVER_PATH,
                false,
            )));
        }

        if config.gpt {
            matchers.push(Box::new(SystemGptMatcher::new(if config.storage_host {
                GptType::StorageHost
            } else {
                GptType::Driver(GPT_DRIVER_PATH)
            })));
        }

        if config.gpt_all {
            matchers.push(Box::new(PartitionMapMatcher::new(
                DiskFormat::Gpt,
                GPT_DRIVER_PATH,
                true,
            )));
        }

        if config.mbr {
            matchers.push(Box::new(PartitionMapMatcher::new(
                DiskFormat::Mbr,
                MBR_DRIVER_PATH,
                true,
            )));
        }

        Matchers { matchers }
    }

    /// Using the set of matchers we created, figure out if this block device matches any of our
    /// expected partitions. If it does, return the information needed to launch the filesystem,
    /// such as the component url or the shared library to pass to the driver binding.
    pub async fn match_device(
        &mut self,
        mut device: Box<dyn Device>,
        env: &mut dyn Environment,
    ) -> Result<bool, Error> {
        // Ramdisks created by fshost can appear in multiple locations.  Only process the first one.
        if let Some(path) = env.registered_devices().get_topological_path(DeviceTag::Ramdisk) {
            let topological_path = device.topological_path();
            if topological_path == path {
                // Exact match, ignore duplicates.
                return Ok(false);
            } else if topological_path.starts_with(&path) {
                // Mark any children of the ramdisk as the fshost ramdisk too.
                device.set_fshost_ramdisk(true);
            }
        }

        for (_, m) in self.matchers.iter_mut().enumerate() {
            if m.match_device(device.as_mut()).await {
                let mut tag = m.process_device(device.as_mut(), env).await?;
                // Tag the first Ramdisk device so that it's retained; the ramdisk will be detached
                // if it's dropped.
                if device.is_fshost_ramdisk() {
                    assert!(tag.is_none());
                    tag = Some(DeviceTag::Ramdisk);
                }
                if let Some(tag) = tag {
                    env.registered_devices().register_device(tag, device);
                    tracing::info!("Registering device {tag:?}");
                }
                return Ok(true);
            }
        }
        Ok(false)
    }
}

// Matches Bootpart devices.
struct BootpartMatcher();

impl BootpartMatcher {
    fn new() -> Self {
        BootpartMatcher()
    }
}

#[async_trait]
impl Matcher for BootpartMatcher {
    async fn match_device(&self, device: &mut dyn Device) -> bool {
        device.get_block_info().await.map_or(false, |info| info.flags.contains(BlockFlag::BOOTPART))
    }

    async fn process_device(
        &mut self,
        device: &mut dyn Device,
        env: &mut dyn Environment,
    ) -> Result<Option<DeviceTag>, Error> {
        env.attach_driver(device, BOOTPART_DRIVER_PATH).await?;
        Ok(None)
    }
}

// Matches Nand devices.
struct NandMatcher();

impl NandMatcher {
    fn new() -> Self {
        NandMatcher()
    }
}

#[async_trait]
impl Matcher for NandMatcher {
    async fn match_device(&self, device: &mut dyn Device) -> bool {
        device.is_nand()
    }

    async fn process_device(
        &mut self,
        device: &mut dyn Device,
        env: &mut dyn Environment,
    ) -> Result<Option<DeviceTag>, Error> {
        env.attach_driver(device, NAND_BROKER_DRIVER_PATH).await?;
        Ok(None)
    }
}

// Matches against a data partition that exists independent of FVM using the DiskFormat.
struct FxblobMatcher {
    // True if this partition is required to exist on a ramdisk.
    ramdisk_required: bool,
    // Because this matcher binds to the system Fxfs component, we can only match on it once.
    // TODO(https://fxbug.dev/42079130): Can we be more precise here, e.g. give the matcher an
    // expected device path based on system configuration?
    already_matched: bool,
}

impl FxblobMatcher {
    fn new(ramdisk_required: bool) -> Self {
        Self { ramdisk_required, already_matched: false }
    }
}

#[async_trait]
impl Matcher for FxblobMatcher {
    async fn match_device(&self, device: &mut dyn Device) -> bool {
        if self.already_matched {
            return false;
        }
        if self.ramdisk_required && !device.is_fshost_ramdisk() {
            return false;
        }
        match device.partition_label().await {
            Ok(label) => {
                // There are a few different labels used depending on the device. If we don't see
                // any of them, this isn't the right partition.
                // TODO(https://fxbug.dev/344018917): Use another mechanism to keep
                // track of partition labels.
                if !DATA_PARTITION_LABELS.contains(&label) {
                    return false;
                }
            }
            // If there is an error getting the partition label, it might be because this device
            // doesn't support labels (like if it's directly on a raw disk in an emulator).
            // Continue with content sniffing.
            Err(_) => (),
        }
        device.content_format().await.ok() == Some(DiskFormat::Fxfs)
    }

    async fn process_device(
        &mut self,
        device: &mut dyn Device,
        env: &mut dyn Environment,
    ) -> Result<Option<DeviceTag>, Error> {
        self.already_matched = true;
        env.mount_fxblob(device).await?;
        env.mount_blob_volume().await?;
        env.mount_data_volume().await?;
        Ok(None)
    }
}

// Matches against FVM partitions, binding them to the new FVM component driver.
struct FvmComponentMatcher {
    // True if this partition is required to exist on a ramdisk.
    ramdisk_required: bool,
    // Because this matcher binds to the system Fvm component, we can only match on it once.
    // TODO(https://fxbug.dev/42079130): Can we be more precise here, e.g. give the matcher an
    // expected device path based on system configuration?
    already_matched: bool,
}

impl FvmComponentMatcher {
    fn new(ramdisk_required: bool) -> Self {
        Self { ramdisk_required, already_matched: false }
    }
}

#[async_trait]
impl Matcher for FvmComponentMatcher {
    async fn match_device(&self, device: &mut dyn Device) -> bool {
        if self.already_matched {
            return false;
        }
        if self.ramdisk_required && !device.is_fshost_ramdisk() {
            return false;
        }
        match device.partition_label().await {
            Ok(label) => {
                // There are a few different labels used depending on the device. If we don't see
                // any of them, this isn't the right partition.
                // TODO(https://fxbug.dev/344018917): Use another mechanism to keep
                // track of partition labels.
                if !(label == FVM_PARTITION_LABEL
                    || label == FUCHSIA_FVM_PARTITION_LABEL
                    || label == FTL_PARTITION_LABEL
                    || label == SUPER_PARTITION_LABEL)
                {
                    tracing::info!("Label {label} doesn't match");
                    return false;
                }
            }
            // If there is an error getting the partition label, it might be because this device
            // doesn't support labels (like if it's directly on a raw disk in an emulator).
            // Continue with content sniffing.
            Err(_) => (),
        }
        device.content_format().await.ok() == Some(DiskFormat::Fvm)
    }

    async fn process_device(
        &mut self,
        device: &mut dyn Device,
        env: &mut dyn Environment,
    ) -> Result<Option<DeviceTag>, Error> {
        self.already_matched = true;
        env.mount_fvm(device).await?;
        env.mount_blob_volume().await?;
        env.mount_data_volume().await?;
        Ok(None)
    }
}

// Matches against the fvm partition and explicitly mounts the data and blob partitions.
// Fails if the blob partition doesn't exist. Creates the data partition if it doesn't
// already exist.
struct FvmMatcher {
    // True if this partition is required to exist on a ramdisk.
    ramdisk_required: bool,

    netboot: bool,

    // Set if we want to mount the blob partition.
    blobfs: bool,

    // Set if we want to mount the data partition.
    data: bool,

    // Set to true if we already matched a partition. It doesn't make sense to try and match
    // multiple main system partitions.
    already_matched: bool,
}

impl FvmMatcher {
    fn new(ramdisk_required: bool, netboot: bool, blobfs: bool, data: bool) -> Self {
        Self { ramdisk_required, netboot, blobfs, data, already_matched: false }
    }
}

#[async_trait]
impl Matcher for FvmMatcher {
    async fn match_device(&self, device: &mut dyn Device) -> bool {
        if self.already_matched {
            return false;
        }
        if self.ramdisk_required && !device.is_fshost_ramdisk() {
            return false;
        }
        device.content_format().await.ok() == Some(DiskFormat::Fvm)
    }

    async fn process_device(
        &mut self,
        device: &mut dyn Device,
        env: &mut dyn Environment,
    ) -> Result<Option<DeviceTag>, Error> {
        // volume names have the format {label}-p-{index}, e.g. blobfs-p-1
        let volume_names = env.bind_and_enumerate_fvm(device).await?;
        if !self.netboot {
            if self.blobfs {
                if let Some(blob_name) =
                    volume_names.iter().find(|name| name.starts_with(BLOBFS_PARTITION_LABEL))
                {
                    env.mount_blobfs_on(blob_name).await?;
                } else {
                    tracing::error!(?volume_names, "Couldn't find blobfs partition!");
                    bail!("Unable to find blobfs within FVM.");
                }
            }
            if self.data {
                if let Some(data_name) = volume_names.iter().find(|name| {
                    name.starts_with(DATA_PARTITION_LABEL)
                        || name.starts_with(LEGACY_DATA_PARTITION_LABEL)
                }) {
                    env.mount_data_on(data_name, device.is_fshost_ramdisk()).await?;
                } else {
                    let fvm_driver_path = format!("{}/fvm", device.topological_path());
                    tracing::warn!(%fvm_driver_path, ?volume_names,
                        "No existing data partition. Calling format_data().",
                    );
                    let fs =
                        env.format_data(&fvm_driver_path).await.context("failed to format data")?;
                    env.bind_data(fs)?;
                }
            }
        }
        // Once we have matched and processed the main system partitions, fuse this matcher so we
        // don't match any other partitions.
        self.already_matched = true;
        Ok(None)
    }
}

enum GptType {
    StorageHost,
    Driver(&'static str),
}

/// Matches the system GPT partition, which is expected to be on a non-removable disk.
struct SystemGptMatcher {
    gpt_type: GptType,
    device_path: Option<String>,
}

impl SystemGptMatcher {
    fn new(gpt_type: GptType) -> Self {
        Self { gpt_type, device_path: None }
    }
}

#[async_trait]
impl Matcher for SystemGptMatcher {
    async fn match_device(&self, device: &mut dyn Device) -> bool {
        if self.device_path.is_some() {
            return false;
        }
        if device.is_nand() || device.is_fshost_ramdisk() {
            return false;
        }
        let removable = device
            .get_block_info()
            .await
            .map(|info| info.flags.contains(BlockFlag::REMOVABLE))
            .inspect_err(|err| {
                tracing::warn!(?err, "Failed to query block info; assuming non-removable device");
            })
            .unwrap_or(false);
        // If the partition has a type GUID, that implies it's inside a partition table so it can't
        // be the system partition table itself.  This is intended to deal with devices like vim3
        // which use the sdmmc partition table and the GPT is one of several sdmmc partitions, but
        // it is reported as having an empty type GUID.
        // NOTE: This is a bit of a hack.  The right way will likely involve a per-board
        // configuration which tells fshost which block device the system partition table is
        // expected to reside in.  For now, this works.
        const EMPTY_GUID: [u8; 16] = [0; 16];
        let has_type_guid = device.partition_type().await.unwrap_or(&EMPTY_GUID) != &EMPTY_GUID;
        // Match the first non-removable device which isn't inside a partition table itself.
        !removable && !has_type_guid
    }

    async fn process_device(
        &mut self,
        device: &mut dyn Device,
        env: &mut dyn Environment,
    ) -> Result<Option<DeviceTag>, Error> {
        match self.gpt_type {
            GptType::Driver(driver_path) => env.attach_driver(device, driver_path).await?,
            GptType::StorageHost => env.launch_gpt_component(device).await?,
        };
        self.device_path = Some(device.topological_path().to_string());
        Ok(Some(DeviceTag::SystemPartitionTable))
    }
}

// Matches partition maps. Matching is done using content sniffing.
struct PartitionMapMatcher {
    // The content format expected.
    content_format: DiskFormat,

    // If true, match against multiple devices. Otherwise, only the first is matched.
    allow_multiple: bool,

    // When matched, this driver is attached to the device.
    driver_path: &'static str,

    // The topological paths of all devices matched so far.
    device_paths: Vec<String>,
}

impl PartitionMapMatcher {
    fn new(content_format: DiskFormat, driver_path: &'static str, allow_multiple: bool) -> Self {
        Self { content_format, allow_multiple, driver_path, device_paths: Vec::new() }
    }
}

#[async_trait]
impl Matcher for PartitionMapMatcher {
    async fn match_device(&self, device: &mut dyn Device) -> bool {
        if !self.allow_multiple && !self.device_paths.is_empty() {
            return false;
        }
        device.content_format().await.ok() == Some(self.content_format)
    }

    async fn process_device(
        &mut self,
        device: &mut dyn Device,
        env: &mut dyn Environment,
    ) -> Result<Option<DeviceTag>, Error> {
        env.attach_driver(device, self.driver_path).await?;
        self.device_paths.push(device.topological_path().to_string());
        Ok(None)
    }
}

// Matches against the first Fxblob partition that isn't the ram-disk
struct FxblobOnRecoveryMatcher {
    // Because this matcher binds to the system Fxfs component, we can only match on it once.
    // TODO(https://fxbug.dev/42079130): Can we be more precise here, e.g. give the matcher an
    // expected device path based on system configuration?
    already_matched: bool,
}

impl FxblobOnRecoveryMatcher {
    fn new() -> Self {
        Self { already_matched: false }
    }
}

#[async_trait]
impl Matcher for FxblobOnRecoveryMatcher {
    async fn match_device(&self, device: &mut dyn Device) -> bool {
        if self.already_matched || device.is_fshost_ramdisk() {
            return false;
        }

        // We only check the partition label and not the content format, because in recovery, the
        // partition might not have any data on it yet (the legacy paver might be about to write to
        // it).
        match device.partition_label().await {
            // There are a few different labels used depending on the device. If we don't see
            // any of them, this isn't the right partition.
            // TODO(https://fxbug.dev/344018917): Use another mechanism to keep
            // track of partition labels.
            Ok(label) if DATA_PARTITION_LABELS.contains(&label) => true,
            _ => false,
        }
    }

    async fn process_device(
        &mut self,
        _device: &mut dyn Device,
        _env: &mut dyn Environment,
    ) -> Result<Option<DeviceTag>, Error> {
        self.already_matched = true;
        Ok(Some(DeviceTag::FxblobOnRecovery))
    }
}

static DATA_PARTITION_LABELS: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    [FVM_PARTITION_LABEL, FUCHSIA_FVM_PARTITION_LABEL, FTL_PARTITION_LABEL, SUPER_PARTITION_LABEL]
        .into()
});

#[cfg(test)]
mod tests {
    use super::{Device, DiskFormat, Environment, Matchers};
    use crate::config::default_config;
    use crate::device::constants::{
        BLOBFS_PARTITION_LABEL, BOOTPART_DRIVER_PATH, DATA_PARTITION_LABEL, FTL_PARTITION_LABEL,
        FUCHSIA_FVM_PARTITION_LABEL, FVM_DRIVER_PATH, FVM_PARTITION_LABEL, GPT_DRIVER_PATH,
        LEGACY_DATA_PARTITION_LABEL, NAND_BROKER_DRIVER_PATH, SUPER_PARTITION_LABEL,
    };
    use crate::device::{DeviceTag, RegisteredDevices};
    use crate::environment::{Filesystem, FilesystemQueue};
    use anyhow::{anyhow, Error};
    use async_trait::async_trait;
    use fidl_fuchsia_device::ControllerProxy;
    use fidl_fuchsia_hardware_block::{BlockInfo, BlockProxy, Flag};
    use fidl_fuchsia_hardware_block_volume::VolumeProxy;
    use fs_management::filesystem::BlockConnector;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct MockDevice {
        block_flags: Flag,
        is_nand: bool,
        content_format: DiskFormat,
        topological_path: String,
        partition_label: Option<String>,
        partition_type: Option<[u8; 16]>,
        is_fshost_ramdisk: bool,
    }

    impl MockDevice {
        fn new() -> Self {
            MockDevice {
                block_flags: Flag::empty(),
                is_nand: false,
                content_format: DiskFormat::Unknown,
                topological_path: "mock_device".to_string(),
                partition_label: None,
                partition_type: None,
                is_fshost_ramdisk: false,
            }
        }
        fn set_block_flags(mut self, flags: Flag) -> Self {
            self.block_flags = flags;
            self
        }
        fn set_nand(mut self, v: bool) -> Self {
            self.is_nand = v;
            self
        }
        fn set_content_format(mut self, format: DiskFormat) -> Self {
            self.content_format = format;
            self
        }
        fn set_topological_path(mut self, path: impl ToString) -> Self {
            self.topological_path = path.to_string().into();
            self
        }
        fn set_partition_label(mut self, label: impl ToString) -> Self {
            self.partition_label = Some(label.to_string());
            self
        }
        fn set_partition_type(mut self, type_guid: [u8; 16]) -> Self {
            self.partition_type = Some(type_guid);
            self
        }
        fn set_fshost_ramdisk(mut self) -> Self {
            self.is_fshost_ramdisk = true;
            self
        }
    }

    #[async_trait]
    impl Device for MockDevice {
        async fn get_block_info(&self) -> Result<fidl_fuchsia_hardware_block::BlockInfo, Error> {
            if self.is_nand {
                Err(anyhow!("not supported by nand device"))
            } else {
                Ok(BlockInfo {
                    block_count: 0,
                    block_size: 0,
                    max_transfer_size: 0,
                    flags: self.block_flags,
                })
            }
        }
        fn is_nand(&self) -> bool {
            self.is_nand
        }
        async fn content_format(&mut self) -> Result<DiskFormat, Error> {
            Ok(self.content_format)
        }
        fn topological_path(&self) -> &str {
            &self.topological_path
        }
        fn path(&self) -> &str {
            &self.topological_path
        }
        async fn partition_label(&mut self) -> Result<&str, Error> {
            match self.partition_label.as_ref() {
                Some(label) => Ok(label.as_str()),
                None => Err(anyhow!("partition label not set")),
            }
        }
        async fn partition_type(&mut self) -> Result<&[u8; 16], Error> {
            self.partition_type.as_ref().ok_or(anyhow!("partition type not set"))
        }
        async fn partition_instance(&mut self) -> Result<&[u8; 16], Error> {
            unreachable!()
        }
        fn controller(&self) -> &ControllerProxy {
            unreachable!()
        }
        fn block_connector(&self) -> Result<Box<dyn BlockConnector>, Error> {
            unreachable!()
        }
        fn block_proxy(&self) -> Result<BlockProxy, Error> {
            unreachable!()
        }
        fn volume_proxy(&self) -> Result<VolumeProxy, Error> {
            unreachable!()
        }
        async fn get_child(&self, _suffix: &str) -> Result<Box<dyn Device>, Error> {
            unreachable!()
        }
        fn is_fshost_ramdisk(&self) -> bool {
            self.is_fshost_ramdisk
        }
        fn set_fshost_ramdisk(&mut self, v: bool) {
            self.is_fshost_ramdisk = v;
        }
    }

    #[derive(Default)]
    struct MockEnv {
        expected_driver_path: Mutex<Option<String>>,
        expect_bind_and_enumerate_fvm: Mutex<bool>,
        expect_mount_blobfs_on: Mutex<bool>,
        expect_mount_fxblob: Mutex<bool>,
        expect_mount_fvm: Mutex<bool>,
        expect_mount_blob_volume: Mutex<bool>,
        expect_mount_data_volume: Mutex<bool>,
        expect_mount_data_on: Mutex<bool>,
        expect_format_data: Mutex<bool>,
        expect_bind_data: Mutex<bool>,
        expect_launch_storage_host: Mutex<bool>,
        legacy_data_format: bool,
        create_data_partition: bool,
        registered_devices: Arc<RegisteredDevices>,
    }

    impl MockEnv {
        fn new() -> Self {
            let mut env = MockEnv::default();
            env.create_data_partition = true;
            env
        }
        fn expect_attach_driver(mut self, path: impl ToString) -> Self {
            *self.expected_driver_path.get_mut().unwrap() = Some(path.to_string());
            self
        }
        fn expect_bind_and_enumerate_fvm(mut self) -> Self {
            *self.expect_bind_and_enumerate_fvm.get_mut().unwrap() = true;
            self
        }
        fn expect_mount_blobfs_on(mut self) -> Self {
            *self.expect_mount_blobfs_on.get_mut().unwrap() = true;
            self
        }
        fn expect_format_data(mut self) -> Self {
            *self.expect_format_data.get_mut().unwrap() = true;
            self
        }
        fn expect_bind_data(mut self) -> Self {
            *self.expect_bind_data.get_mut().unwrap() = true;
            self
        }
        fn expect_mount_fxblob(mut self) -> Self {
            *self.expect_mount_fxblob.get_mut().unwrap() = true;
            self
        }
        fn expect_mount_fvm(mut self) -> Self {
            *self.expect_mount_fvm.get_mut().unwrap() = true;
            self
        }
        fn expect_mount_blob_volume(mut self) -> Self {
            *self.expect_mount_blob_volume.get_mut().unwrap() = true;
            self
        }
        fn expect_mount_data_volume(mut self) -> Self {
            *self.expect_mount_data_volume.get_mut().unwrap() = true;
            self
        }
        fn expect_mount_data_on(mut self) -> Self {
            *self.expect_mount_data_on.get_mut().unwrap() = true;
            self
        }
        fn expect_launch_storage_host(mut self) -> Self {
            *self.expect_launch_storage_host.get_mut().unwrap() = true;
            self
        }
        fn legacy_data_format(mut self) -> Self {
            self.legacy_data_format = true;
            self
        }
        fn without_data_partition(mut self) -> Self {
            self.create_data_partition = false;
            self
        }
    }

    #[async_trait]
    impl Environment for MockEnv {
        async fn attach_driver(
            &self,
            _device: &mut dyn Device,
            driver_path: &str,
        ) -> Result<(), Error> {
            assert_eq!(
                driver_path,
                self.expected_driver_path
                    .lock()
                    .unwrap()
                    .take()
                    .expect("Unexpected call to attach_driver")
            );
            Ok(())
        }

        async fn launch_gpt_component(&mut self, _device: &mut dyn Device) -> Result<(), Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_launch_storage_host.lock().unwrap()),
                true,
                "Unexpected call to launch_storage_host"
            );
            Ok(())
        }

        fn partition_manager_exposed_dir(
            &mut self,
        ) -> Result<fidl_fuchsia_io::DirectoryProxy, Error> {
            unreachable!()
        }

        async fn bind_and_enumerate_fvm(
            &mut self,
            _device: &mut dyn Device,
        ) -> Result<Vec<String>, Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_bind_and_enumerate_fvm.lock().unwrap()),
                true,
                "Unexpected call to bind_and_enumerate_fvm"
            );
            let mut volume_names = vec![BLOBFS_PARTITION_LABEL.to_string()];
            if self.create_data_partition {
                if self.legacy_data_format {
                    volume_names.push(LEGACY_DATA_PARTITION_LABEL.to_string())
                } else {
                    volume_names.push(DATA_PARTITION_LABEL.to_string())
                };
            }
            Ok(volume_names)
        }

        async fn mount_blobfs_on(&mut self, _blobfs_partition_name: &str) -> Result<(), Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_mount_blobfs_on.lock().unwrap()),
                true,
                "Unexpected call to mount_blobfs_on"
            );
            Ok(())
        }

        async fn mount_data_on(
            &mut self,
            _data_partition_name: &str,
            _is_fshost_ramdisk: bool,
        ) -> Result<(), Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_mount_data_on.lock().unwrap()),
                true,
                "Unexpected call to mount_data_on"
            );
            Ok(())
        }

        async fn format_data(&mut self, _fvm_topo_path: &str) -> Result<Filesystem, Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_format_data.lock().unwrap()),
                true,
                "Unexpected call to format_data"
            );
            Ok(Filesystem::Queue(FilesystemQueue::default()))
        }

        fn bind_data(&mut self, mut _fs: Filesystem) -> Result<(), Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_bind_data.lock().unwrap()),
                true,
                "Unexpected call to bind_data"
            );
            Ok(())
        }

        async fn mount_fxblob(&mut self, _device: &mut dyn Device) -> Result<(), Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_mount_fxblob.lock().unwrap()),
                true,
                "Unexpected call to mount_fxblob"
            );
            Ok(())
        }

        async fn mount_fvm(&mut self, _device: &mut dyn Device) -> Result<(), Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_mount_fvm.lock().unwrap()),
                true,
                "Unexpected call to mount_fxblob"
            );
            Ok(())
        }

        async fn mount_blob_volume(&mut self) -> Result<(), Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_mount_blob_volume.lock().unwrap()),
                true,
                "Unexpected call to mount_blob_volume"
            );
            Ok(())
        }

        async fn mount_data_volume(&mut self) -> Result<(), Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_mount_data_volume.lock().unwrap()),
                true,
                "Unexpected call to mount_data_volume"
            );
            Ok(())
        }

        async fn shred_data(&mut self) -> Result<(), Error> {
            unreachable!();
        }

        async fn shutdown(&mut self) -> Result<(), Error> {
            unreachable!();
        }

        fn registered_devices(&self) -> &Arc<RegisteredDevices> {
            &self.registered_devices
        }
    }

    impl Drop for MockEnv {
        fn drop(&mut self) {
            assert!(self.expected_driver_path.get_mut().unwrap().is_none());
            assert!(!*self.expect_mount_blobfs_on.lock().unwrap());
            assert!(!*self.expect_mount_data_on.lock().unwrap());
            assert!(!*self.expect_bind_and_enumerate_fvm.lock().unwrap());
            assert!(!*self.expect_bind_data.lock().unwrap());
            assert!(!*self.expect_mount_fxblob.lock().unwrap());
            assert!(!*self.expect_mount_fvm.lock().unwrap());
            assert!(!*self.expect_mount_blob_volume.lock().unwrap());
            assert!(!*self.expect_mount_data_volume.lock().unwrap());
            assert!(!*self.expect_format_data.lock().unwrap());
            assert!(!*self.expect_launch_storage_host.lock().unwrap());
        }
    }

    #[fuchsia::test]
    async fn test_bootpart_matcher() {
        let mock_device = MockDevice::new().set_block_flags(Flag::BOOTPART);

        // Check no match when disabled in config.
        assert!(!Matchers::new(&fshost_config::Config {
            bootpart: false,
            gpt: false,
            ..default_config()
        },)
        .match_device(Box::new(mock_device.clone()), &mut MockEnv::new())
        .await
        .expect("match_device failed"));

        assert!(Matchers::new(&default_config())
            .match_device(
                Box::new(mock_device),
                &mut MockEnv::new().expect_attach_driver(BOOTPART_DRIVER_PATH)
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_nand_matcher() {
        let device = MockDevice::new().set_nand(true);
        let mut env = MockEnv::new().expect_attach_driver(NAND_BROKER_DRIVER_PATH);

        // Default shouldn't match.
        assert!(!Matchers::new(&default_config())
            .match_device(Box::new(device.clone()), &mut env)
            .await
            .expect("match_device failed"));

        assert!(Matchers::new(&fshost_config::Config { nand: true, ..default_config() })
            .match_device(Box::new(device), &mut env)
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_partition_map_matcher() {
        let mut env = MockEnv::new().expect_attach_driver(GPT_DRIVER_PATH);

        // Check no match when disabled in config.
        let device = MockDevice::new().set_content_format(DiskFormat::Gpt);
        assert!(!Matchers::new(&fshost_config::Config {
            blobfs: false,
            data: false,
            gpt: false,
            ..default_config()
        },)
        .match_device(Box::new(device.clone()), &mut env)
        .await
        .expect("match_device failed"));

        let mut matchers = Matchers::new(&default_config());
        assert!(matchers
            .match_device(Box::new(device.clone()), &mut env)
            .await
            .expect("match_device failed"));

        // More GPT devices should not get matched.
        assert!(!matchers
            .match_device(Box::new(device.clone()), &mut env)
            .await
            .expect("match_device failed"));

        // The gpt_all config should allow multiple GPT devices to be matched.
        let mut matchers =
            Matchers::new(&fshost_config::Config { gpt_all: true, ..default_config() });
        let mut env = MockEnv::new().expect_attach_driver(GPT_DRIVER_PATH);
        assert!(matchers
            .match_device(Box::new(device.clone()), &mut env)
            .await
            .expect("match_device failed"));
        let mut env = MockEnv::new().expect_attach_driver(GPT_DRIVER_PATH);
        assert!(matchers
            .match_device(Box::new(device.clone()), &mut env)
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_partition_map_matcher_ramdisk() {
        // If ramdisk_image is true and one of the devices matches the ramdisk prefix, we will match
        // two fvm devices, and the third one will fail.
        let mut matchers = Matchers::new(&fshost_config::Config {
            ramdisk_image: true,
            data_filesystem_format: "minfs".to_string(),
            gpt: false,
            ..default_config()
        });
        let fvm_device = MockDevice::new()
            .set_content_format(DiskFormat::Fvm)
            .set_topological_path("first_prefix");
        let mut env = MockEnv::new().expect_attach_driver(FVM_DRIVER_PATH);
        assert!(matchers
            .match_device(Box::new(fvm_device.clone()), &mut env)
            .await
            .expect("match_device failed"));

        let fvm_device = fvm_device.set_topological_path("second_prefix").set_fshost_ramdisk();
        let mut env = MockEnv::new()
            .expect_bind_and_enumerate_fvm()
            .expect_mount_blobfs_on()
            .expect_mount_data_on();
        assert!(matchers
            .match_device(Box::new(fvm_device.clone()), &mut env)
            .await
            .expect("match_device failed"));

        let fvm_device = fvm_device.set_topological_path("third_prefix");
        assert!(!matchers
            .match_device(Box::new(fvm_device), &mut MockEnv::new())
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn partition_map_matcher_wrong_prefix_match() {
        // If ramdisk_image is true but no devices match the prefix, only the first device will
        // match.
        let mut matchers = Matchers::new(&fshost_config::Config {
            ramdisk_image: true,
            data_filesystem_format: "fxfs".to_string(),
            gpt: false,
            ..default_config()
        });

        let fvm_device = MockDevice::new()
            .set_content_format(DiskFormat::Fvm)
            .set_topological_path("first_prefix");
        let mut env = MockEnv::new().expect_attach_driver(FVM_DRIVER_PATH);
        assert!(matchers
            .match_device(Box::new(fvm_device.clone()), &mut env)
            .await
            .expect("match_device failed"));
        let fvm_device = fvm_device.set_topological_path("second_prefix");
        assert!(!matchers
            .match_device(Box::new(fvm_device), &mut MockEnv::new())
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn partition_map_matcher_no_prefix_match() {
        // If ramdisk_image is true but no ramdisk path is provided, only the first device will
        // match.
        let mut matchers = Matchers::new(&fshost_config::Config {
            ramdisk_image: true,
            data_filesystem_format: "fxfs".to_string(),
            gpt: false,
            ..default_config()
        });

        let fvm_device = MockDevice::new()
            .set_content_format(DiskFormat::Fvm)
            .set_topological_path("first_prefix");
        let mut env = MockEnv::new().expect_attach_driver(FVM_DRIVER_PATH);
        assert!(matchers
            .match_device(Box::new(fvm_device.clone()), &mut env)
            .await
            .expect("match_device failed"));
        let fvm_device = fvm_device.set_topological_path("second_prefix");
        assert!(!matchers
            .match_device(Box::new(fvm_device), &mut MockEnv::new())
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_blobfs_matcher() {
        let fvm_device = MockDevice::new().set_content_format(DiskFormat::Fvm);
        let mut env = MockEnv::new().expect_bind_and_enumerate_fvm().expect_mount_blobfs_on();

        let mut matchers =
            Matchers::new(&fshost_config::Config { data: false, ..default_config() });

        assert!(matchers
            .match_device(Box::new(fvm_device), &mut env)
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_data_matcher() {
        let mut matchers =
            Matchers::new(&fshost_config::Config { blobfs: false, ..default_config() });

        assert!(matchers
            .match_device(
                Box::new(MockDevice::new().set_content_format(DiskFormat::Fvm)),
                &mut MockEnv::new().expect_bind_and_enumerate_fvm().expect_mount_data_on()
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_legacy_data_matcher() {
        let mut matchers = Matchers::new(&default_config());

        assert!(matchers
            .match_device(
                Box::new(MockDevice::new().set_content_format(DiskFormat::Fvm)),
                &mut MockEnv::new()
                    .legacy_data_format()
                    .expect_bind_and_enumerate_fvm()
                    .expect_mount_blobfs_on()
                    .expect_mount_data_on()
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_matcher_without_data_partition() {
        let mut matchers = Matchers::new(&default_config());

        assert!(matchers
            .match_device(
                Box::new(MockDevice::new().set_content_format(DiskFormat::Fvm)),
                &mut MockEnv::new()
                    .without_data_partition()
                    .expect_bind_and_enumerate_fvm()
                    .expect_mount_blobfs_on()
                    .expect_format_data()
                    .expect_bind_data()
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_multiple_fvm_partitions_no_label() {
        let mut matchers = Matchers::new(&fshost_config::Config { gpt: false, ..default_config() });

        assert!(matchers
            .match_device(
                Box::new(MockDevice::new().set_content_format(DiskFormat::Fvm)),
                &mut MockEnv::new()
                    .expect_bind_and_enumerate_fvm()
                    .expect_mount_data_on()
                    .expect_mount_blobfs_on()
            )
            .await
            .expect("match_device failed"));

        assert!(!matchers
            .match_device(
                Box::new(MockDevice::new().set_content_format(DiskFormat::Fvm)),
                &mut MockEnv::new()
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_netboot_flag_true() {
        let mut matchers =
            Matchers::new(&fshost_config::Config { netboot: true, ..default_config() });

        assert!(matchers
            .match_device(
                Box::new(MockDevice::new().set_content_format(DiskFormat::Fvm)),
                &mut MockEnv::new().expect_bind_and_enumerate_fvm()
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_netboot_flag_true_fxblob() {
        let mut matchers = Matchers::new(&fshost_config::Config {
            data_filesystem_format: "fxfs".to_string(),
            netboot: true,
            fxfs_blob: true,
            ..default_config()
        });

        assert!(matchers
            .match_device(
                Box::new(MockDevice::new().set_content_format(DiskFormat::Gpt)),
                &mut MockEnv::new().expect_attach_driver(GPT_DRIVER_PATH)
            )
            .await
            .expect("match_device failed"));

        // FVM shouldn't match...
        assert!(!matchers
            .match_device(
                Box::new(MockDevice::new().set_content_format(DiskFormat::Fvm)),
                &mut MockEnv::new()
            )
            .await
            .expect("match_device failed"));

        // Fxblob should match, but not try and mount.
        assert!(matchers
            .match_device(
                Box::new(
                    MockDevice::new()
                        .set_content_format(DiskFormat::Fxfs)
                        .set_partition_label(FVM_PARTITION_LABEL)
                ),
                &mut MockEnv::new()
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_fxblob_matcher() {
        let mut matchers = Matchers::new(&fshost_config::Config {
            fxfs_blob: true,
            data_filesystem_format: "fxfs".to_string(),
            gpt: false,
            ..default_config()
        });

        // A device with the wrong label should fail.
        assert!(!matchers
            .match_device(
                Box::new(
                    MockDevice::new()
                        .set_content_format(DiskFormat::Fxfs)
                        .set_partition_label("wrong_label")
                ),
                &mut MockEnv::new()
            )
            .await
            .expect("match_device failed"));

        // A device with the right label should succeed.
        assert!(matchers
            .match_device(
                Box::new(
                    MockDevice::new()
                        .set_content_format(DiskFormat::Fxfs)
                        .set_partition_label(FVM_PARTITION_LABEL)
                ),
                &mut MockEnv::new()
                    .expect_mount_fxblob()
                    .expect_mount_blob_volume()
                    .expect_mount_data_volume()
            )
            .await
            .expect("match_device failed"));

        // We should only be able to match Fxblob once.
        assert!(!matchers
            .match_device(
                Box::new(
                    MockDevice::new()
                        .set_content_format(DiskFormat::Fxfs)
                        .set_partition_label(FVM_PARTITION_LABEL)
                ),
                &mut MockEnv::new(),
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_fvm_component_matcher() {
        let new_matchers = || {
            Matchers::new(&fshost_config::Config {
                storage_host: true,
                fvm: true,
                data_filesystem_format: "minfs".to_string(),
                gpt: false,
                ..default_config()
            })
        };

        let mut matchers = new_matchers();

        // A device with the wrong label should fail.
        assert!(!matchers
            .match_device(
                Box::new(
                    MockDevice::new()
                        .set_content_format(DiskFormat::Fvm)
                        .set_partition_label("wrong_label")
                ),
                &mut MockEnv::new()
            )
            .await
            .expect("match_device failed"));

        // A device with the right label should succeed.
        for label in [
            FVM_PARTITION_LABEL,
            FUCHSIA_FVM_PARTITION_LABEL,
            FTL_PARTITION_LABEL,
            SUPER_PARTITION_LABEL,
        ] {
            matchers = new_matchers();
            assert!(matchers
                .match_device(
                    Box::new(
                        MockDevice::new()
                            .set_content_format(DiskFormat::Fvm)
                            .set_partition_label(label)
                    ),
                    &mut MockEnv::new()
                        .expect_mount_fvm()
                        .expect_mount_blob_volume()
                        .expect_mount_data_volume()
                )
                .await
                .expect("match_device failed"));
        }

        // We should only be able to match Fvm once.
        assert!(!matchers
            .match_device(
                Box::new(
                    MockDevice::new()
                        .set_content_format(DiskFormat::Fvm)
                        .set_partition_label(FVM_PARTITION_LABEL)
                ),
                &mut MockEnv::new(),
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_fxblob_matcher_alternate_label() {
        let mut matchers = Matchers::new(&fshost_config::Config {
            fxfs_blob: true,
            data_filesystem_format: "fxfs".to_string(),
            gpt: false,
            ..default_config()
        });

        // A device with the wrong label should fail.
        assert!(!matchers
            .match_device(
                Box::new(
                    MockDevice::new()
                        .set_content_format(DiskFormat::Fxfs)
                        .set_partition_label("wrong_label")
                ),
                &mut MockEnv::new()
            )
            .await
            .expect("match_device failed"));

        // A device with the right label should succeed.
        assert!(matchers
            .match_device(
                Box::new(
                    MockDevice::new()
                        .set_content_format(DiskFormat::Fxfs)
                        .set_partition_label(FUCHSIA_FVM_PARTITION_LABEL)
                ),
                &mut MockEnv::new()
                    .expect_mount_fxblob()
                    .expect_mount_blob_volume()
                    .expect_mount_data_volume()
            )
            .await
            .expect("match_device failed"));

        // We should only be able to match Fxblob once.
        assert!(!matchers
            .match_device(
                Box::new(
                    MockDevice::new()
                        .set_content_format(DiskFormat::Fxfs)
                        .set_partition_label(FUCHSIA_FVM_PARTITION_LABEL)
                ),
                &mut MockEnv::new(),
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    fn test_device_fvm_path() {
        let device =
            MockDevice::new().set_topological_path("/some/fvm/path/with/another/fvm/inside");
        assert_eq!(device.fvm_path(), Some("/some/fvm/path/with/another/fvm".to_string()));
    }

    #[fuchsia::test]
    async fn test_fxblob_matcher_without_label() {
        let mut matchers = Matchers::new(&fshost_config::Config {
            fxfs_blob: true,
            data_filesystem_format: "fxfs".to_string(),
            gpt: false,
            ..default_config()
        });

        assert!(matchers
            .match_device(
                Box::new(MockDevice::new().set_content_format(DiskFormat::Fxfs)),
                &mut MockEnv::new()
                    .expect_mount_fxblob()
                    .expect_mount_blob_volume()
                    .expect_mount_data_volume()
            )
            .await
            .expect("match_device failed"));

        // We should only be able to match Fxblob once.
        assert!(!matchers
            .match_device(
                Box::new(MockDevice::new().set_content_format(DiskFormat::Fxfs)),
                &mut MockEnv::new(),
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_storage_host_matcher() {
        let mut matchers =
            Matchers::new(&fshost_config::Config { storage_host: true, ..default_config() });

        // Don't match devices with a partition type, since they are likely nested in another GPT.
        assert!(!matchers
            .match_device(
                Box::new(
                    MockDevice::new()
                        .set_content_format(DiskFormat::Gpt)
                        .set_partition_type([1u8; 16])
                ),
                &mut MockEnv::new()
            )
            .await
            .expect("match_device failed"));

        assert!(matchers
            .match_device(
                Box::new(MockDevice::new()),
                &mut MockEnv::new().expect_launch_storage_host()
            )
            .await
            .expect("match_device failed"));

        // Any future devices shouldn't bind.
        assert!(!matchers
            .match_device(
                Box::new(MockDevice::new().set_content_format(DiskFormat::Gpt)),
                &mut MockEnv::new()
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_fxblob_on_recovery_matcher() {
        let mut matchers = Matchers::new(&fshost_config::Config {
            storage_host: true,
            ramdisk_image: true,
            fxfs_blob: true,
            ..default_config()
        });

        // The non-ramdisk should match.
        let mut env = MockEnv::new();
        assert!(matchers
            .match_device(
                Box::new(MockDevice::new().set_partition_label(FUCHSIA_FVM_PARTITION_LABEL)),
                &mut env
            )
            .await
            .expect("match_device failed"));

        assert!(env.registered_devices.get_topological_path(DeviceTag::FxblobOnRecovery).is_some());

        let mut env =
            env.expect_mount_fxblob().expect_mount_blob_volume().expect_mount_data_volume();

        // And the ramdisk Fxblob should too.
        assert!(matchers
            .match_device(
                Box::new(
                    MockDevice::new().set_content_format(DiskFormat::Fxfs).set_fshost_ramdisk()
                ),
                &mut env
            )
            .await
            .expect("match_device failed"));

        assert!(env.registered_devices.get_topological_path(DeviceTag::Ramdisk).is_some());
    }
}
