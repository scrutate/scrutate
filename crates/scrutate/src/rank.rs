//! Learned ranker: a LightGBM tree ensemble exported to plain arrays (`python/scrutate_ref/rank.py` writes them).
//! Training happens offline (Python toolkit); here only the traversal is needed.

use std::path::Path;

use anyhow::{Context, Result, anyhow};

use crate::npy::{load_npz, load_npz_bytes};

pub struct TreeEnsemble {
    feat: Vec<i32>, // [T, m]; leaves have feat < 0
    thr: Vec<f64>,
    left: Vec<i32>,
    right: Vec<i32>,
    val: Vec<f32>,
    pub feature_names: Vec<String>,
    base: f64,
    trees: usize,
    m: usize,
}

impl TreeEnsemble {
    pub fn load(path: &Path) -> Result<TreeEnsemble> {
        let arrays = load_npz(path).with_context(|| format!("load ranker {}", path.display()))?;
        Self::from_arrays(arrays)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<TreeEnsemble> {
        Self::from_arrays(load_npz_bytes(bytes).context("load embedded ranker")?)
    }

    fn from_arrays(arrays: Vec<(String, crate::npy::Npy)>) -> Result<TreeEnsemble> {
        let mut map: std::collections::HashMap<String, crate::npy::Npy> = arrays.into_iter().collect();
        let mut take = |n: &str| map.remove(n).ok_or_else(|| anyhow!("ranker lacks {n}"));
        let feat_a = take("feat")?;
        let (trees, m) = (feat_a.shape[0], feat_a.shape[1]);
        Ok(TreeEnsemble {
            feat: feat_a.to_vec::<i32>()?,
            thr: take("thr")?.to_vec::<f64>()?,
            left: take("left")?.to_vec::<i32>()?,
            right: take("right")?.to_vec::<i32>()?,
            val: take("val")?.to_vec::<f32>()?,
            feature_names: take("names")?.to_strings()?,
            base: take("base")?.to_vec::<f64>()?.first().copied().unwrap_or(0.0),
            trees,
            m,
        })
    }

    /// Score of one feature row (LightGBM semantics: `x <= threshold` goes left, NaN goes right).
    pub fn predict_row(&self, x: &[f64]) -> f64 {
        let mut s = 0.0f64; // leaves summed tree by tree, then the base: one order, so a score has the same bits everywhere
        for t in 0..self.trees {
            let base = t * self.m;
            let mut i = 0usize;
            loop {
                let f = self.feat[base + i];
                if f < 0 {
                    s += self.val[base + i] as f64;
                    break;
                }
                i = if x[f as usize] <= self.thr[base + i] { self.left[base + i] as usize } else { self.right[base + i] as usize };
            }
        }
        s + self.base
    }

    pub fn predict(&self, rows: &[Vec<f64>]) -> Vec<f64> {
        rows.iter().map(|r| self.predict_row(r)).collect()
    }
}
