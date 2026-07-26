use std::path::Path;

use crate::error::{Error, Result};

use super::{PatchPlan, PATCH_PLAN_NAME};
use crate::runtime::griffr_patch_path;

pub(crate) fn write_patch_plan(plan: &PatchPlan) -> Result<()> {
    plan.validate()?;
    let patch_dir = griffr_patch_path(&plan.install_root);
    std::fs::create_dir_all(&patch_dir).map_err(|source| Error::IoAt {
        action: "create directory",
        path: patch_dir.clone(),
        source,
    })?;
    let path = patch_dir.join(PATCH_PLAN_NAME);
    let payload = serde_json::to_vec_pretty(plan)?;
    crate::runtime::task_pool::fs_ops::write_atomic_bytes(&path, &payload)
}

pub(crate) fn read_patch_plan(install_root: &Path) -> Result<PatchPlan> {
    let path = griffr_patch_path(install_root).join(PATCH_PLAN_NAME);
    let plan: PatchPlan =
        serde_json::from_slice(&std::fs::read(&path).map_err(|source| Error::IoAt {
            action: "open file",
            path: path.clone(),
            source,
        })?)?;
    plan.validate()?;
    Ok(plan)
}
