//! Cooking a [`Project`]: the derived data cache under it and a whole cook through given
//! cookers. Shared like `project.rs` (see there): the cook crates include this file by path; the
//! including crate also needs marlin-cook-core and marlin-serial.
#![allow(dead_code, reason = "each test binary uses a subset")]

use marlin_cook_core::{CookOptions, Cookers, Ddc, LocalDdc, cook_all};
use marlin_serial::cooked::CookedBuffer;

use super::Project;

/// A cooked unit: its asset path, bytes (page-aligned) and warnings.
#[derive(Debug)]
pub struct CookedUnit {
    pub path: String,
    pub bytes: CookedBuffer,
    pub warnings: Vec<String>,
}

impl Project {
    /// The local cache at `.marlin/<name>`.
    pub fn ddc(&self, name: &str) -> LocalDdc {
        LocalDdc::open(self.root.join(".marlin").join(name)).unwrap()
    }

    /// Indexes and cooks everything into `.marlin/ddc`; returns the units sorted by path, or the
    /// errors.
    pub fn cook(&self, cookers: &Cookers) -> Result<Vec<CookedUnit>, Vec<String>> {
        let db = self.db();
        let ddc = self.ddc("ddc");
        let (report, _) = cook_all(&db, cookers, &ddc, &CookOptions::default());
        if !report.errors.is_empty() {
            return Err(report.errors.iter().map(|e| e.to_string()).collect());
        }
        let mut units: Vec<CookedUnit> = report
            .units
            .iter()
            .map(|u| CookedUnit {
                path: u.path.clone(),
                bytes: CookedBuffer::copy_from(&ddc.get(u.key).unwrap().unwrap(), 4096),
                warnings: u.warnings.clone(),
            })
            .collect();
        units.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(units)
    }
}
