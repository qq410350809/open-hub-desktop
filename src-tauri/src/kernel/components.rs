use serde::Serialize;
use std::sync::Arc;

use crate::context::{AppContext, Managed};
use crate::kernel::{
    get_geoip_status_impl, get_mihomo_kernel_status_impl, GeoipStatus, MihomoKernelStatus,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentBootstrapStatus {
    pub mihomo: MihomoKernelStatus,
    pub geoip: GeoipStatus,
}

pub async fn get_component_bootstrap_status_impl(
    ctx: &Arc<AppContext>,
) -> Result<ComponentBootstrapStatus, String> {
    Ok(ComponentBootstrapStatus {
        mihomo: get_mihomo_kernel_status_impl(ctx).await?,
        geoip: get_geoip_status_impl(ctx).await?,
    })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_component_bootstrap_status(
    ctx: Managed<'_, Arc<AppContext>>,
) -> Result<ComponentBootstrapStatus, String> {
    get_component_bootstrap_status_impl(&ctx).await
}
