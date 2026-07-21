use std::path::Path;

use crate::error::{Error, Result};

use super::{PatchPlan, PATCH_PLAN_NAME, PATCH_TRANSACTION_DIR};

pub(crate) fn write_patch_plan(plan: &PatchPlan) -> Result<()> {
    plan.validate()?;
    let transaction_dir = plan.install_root.join(PATCH_TRANSACTION_DIR);
    std::fs::create_dir_all(&transaction_dir).map_err(|source| Error::CreateDirFailed {
        path: transaction_dir.clone(),
        source,
    })?;
    let path = transaction_dir.join(PATCH_PLAN_NAME);
    let temp = transaction_dir.join(format!("{PATCH_PLAN_NAME}.tmp"));
    std::fs::write(&temp, serde_json::to_vec_pretty(plan)?).map_err(|source| {
        Error::OpenFileFailed {
            path: temp.clone(),
            source,
        }
    })?;
    crate::runtime::task_pool::fs_ops::extract::move_path_replace(&temp, &path)
}

pub(crate) fn read_patch_plan(install_root: &Path) -> Result<PatchPlan> {
    let path = install_root
        .join(PATCH_TRANSACTION_DIR)
        .join(PATCH_PLAN_NAME);
    let plan: PatchPlan =
        serde_json::from_slice(
            &std::fs::read(&path).map_err(|source| Error::OpenFileFailed {
                path: path.clone(),
                source,
            })?,
        )?;
    plan.validate()?;
    Ok(plan)
}
