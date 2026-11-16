//! Learned ranker: a LightGBM tree ensemble exported to plain arrays (`python/scrutate_ref/rank.py` writes them).
//! Training happens offline (Python toolkit); here only the traversal is needed.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

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
        let &[trees, m] = feat_a.shape.as_slice() else { bail!("ranker feat is not [trees, nodes]") };
        let e = TreeEnsemble {
            feat: feat_a.to_vec::<i32>()?,
            thr: take("thr")?.to_vec::<f64>()?,
            left: take("left")?.to_vec::<i32>()?,
            right: take("right")?.to_vec::<i32>()?,
            val: take("val")?.to_vec::<f32>()?,
            feature_names: take("names")?.to_strings()?,
            base: take("base")?.to_vec::<f64>()?.first().copied().unwrap_or(0.0),
            trees,
            m,
        };
        let n = trees * m;
        if [e.feat.len(), e.thr.len(), e.left.len(), e.right.len(), e.val.len()] != [n; 5] {
            bail!("ranker arrays are not all {trees} trees of {m} nodes");
        }
        // a split reads a feature of the row and goes to a later node of its tree, so every walk ends
        for (k, &f) in e.feat.iter().enumerate().filter(|&(_, &f)| f >= 0) {
            let later = |c: i32| (k % m) < c as usize && (c as usize) < m;
            if f as usize >= e.feature_names.len() || !later(e.left[k]) || !later(e.right[k]) {
                bail!("ranker node {} of tree {} splits on feature {f} or points outside its tree", k % m, k / m);
            }
        }
        Ok(e)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bundled() -> Vec<(String, crate::npy::Npy)> {
        crate::npy::load_npz(&Path::new(env!("CARGO_MANIFEST_DIR")).join("models/ranker.npz")).unwrap()
    }

    /// The bundled arrays with `name` replaced by what `edit` makes of its values.
    fn with<T: crate::npy::Pod>(name: &str, edit: impl Fn(&mut Vec<T>, &TreeEnsemble) -> Vec<usize>) -> Result<TreeEnsemble> {
        let good = TreeEnsemble::from_arrays(bundled()).unwrap();
        let mut arrays = bundled();
        let i = arrays.iter().position(|(n, _)| n == name).unwrap();
        let mut v = arrays[i].1.to_vec::<T>().unwrap();
        let shape = edit(&mut v, &good);
        let tmp = std::env::temp_dir().join(format!("scrutate_ranker_{name}_{}.npy", std::process::id()));
        crate::npy::save(&tmp, &v, &shape).unwrap();
        arrays[i].1 = crate::npy::Npy::open(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        TreeEnsemble::from_arrays(arrays)
    }

    #[test]
    fn a_ranker_whose_arrays_do_not_fit_is_refused() {
        assert!(TreeEnsemble::from_arrays(bundled()).is_ok(), "the bundled ranker loads");
        let cut = with::<f64>("thr", |v, _| {
            v.truncate(5);
            vec![5]
        });
        assert!(cut.is_err(), "thr cut to 5 values");
        // a split on a feature past the row
        let past = with::<i32>("feat", |v, e| {
            let at = v.iter().position(|&f| f >= 0).unwrap();
            v[at] = e.feature_names.len() as i32;
            vec![e.trees, e.m]
        });
        assert!(past.is_err(), "a feature past the row");
        // a child past the tree, and one back at the root: a loop
        for child in [i32::MAX, 0] {
            let bad = with::<i32>("left", |v, e| {
                v[0] = child;
                vec![e.trees, e.m]
            });
            assert!(bad.is_err(), "left child {child}");
        }
    }
}
