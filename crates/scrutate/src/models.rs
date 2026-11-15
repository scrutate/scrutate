//! The model set: the vector model, the learned ranker and the feature list the ranker was fitted
//! on, with one revision. Each part loads on its first use, once per process, so an operation that
//! reads no vector and ranks nothing never unpacks either file. A ranker whose features are not
//! this engine's is an error when something ranks with it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, Result};

use crate::rank::TreeEnsemble;
use crate::semantic::SemanticModel;

/// The vector model the stored vectors are made with. Bump it with every change of the embedded
/// model (its signature changes then too): an index of a higher revision is refused, since its
/// vectors do not match the queries this binary encodes.
pub const REVISION: u32 = 2;

/// The ranking features, in the order the ranker reads them.
pub const FEATURES: [&str; 48] = [
    "lex_log",
    "lex_ratio",
    "lex_rank",
    "lex_z",
    "name_log",
    "name_ratio",
    "qual_log",
    "qual_ratio",
    "sig_log",
    "sig_ratio",
    "doc_log",
    "doc_ratio",
    "body_log",
    "body_ratio",
    "path_log",
    "path_ratio",
    "sem_a",
    "sem_b",
    "sem_mix",
    "sem_a_z",
    "sem_b_z",
    "sem_mix_z",
    "sem_rank",
    "exact_name",
    "exact_qual",
    "name_overlap",
    "name_cover",
    "name_same_set",
    "lit_hits",
    "lit_def",
    "k_function",
    "k_method",
    "k_class",
    "k_module",
    "k_other",
    "is_test",
    "log_nbody",
    "has_doc",
    "log_ndoc",
    "qual_depth",
    "is_private",
    "log_ncallers",
    "n_terms",
    "n_lits",
    "n_words",
    "has_ident",
    "file_frac",
    "exact_case",
];

static SEMANTIC_NPZ: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/models/semantic.npz"));
static RANKER_NPZ: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/models/ranker.npz"));
/// The signature of the bundled vector model, so no process hashes 7.6 MB; a test prints the new
/// value when the file changes (a hashing build dependency would rename the order file's symbols).
pub const BUNDLED_SIGNATURE: &str = "blake2b:f0f48afc6df71783a8c6dfb5048ed440";

enum Source {
    Bundled,
    File(PathBuf),
}

impl Source {
    /// The bundled part, or the file a variable names; `none` or empty turns the part off.
    fn from_var(var: &str) -> Option<Source> {
        match std::env::var_os(var) {
            None => Some(Source::Bundled),
            Some(v) if v.is_empty() || v == "none" => None,
            Some(v) => Some(Source::File(PathBuf::from(v))),
        }
    }
}

struct Part<T> {
    source: Source,
    loaded: OnceLock<std::result::Result<T, String>>,
}

impl<T> Part<T> {
    fn new(source: Source) -> Part<T> {
        Part { source, loaded: OnceLock::new() }
    }

    fn get(&self, load: impl FnOnce(&Source) -> Result<T>) -> Result<&T> {
        self.loaded.get_or_init(|| load(&self.source).map_err(|e| format!("{e:#}"))).as_ref().map_err(|e| anyhow!("{e}"))
    }
}

pub struct ModelSet {
    semantic: Option<Part<SemanticModel>>,
    ranker: Option<Part<TreeEnsemble>>,
}

impl ModelSet {
    /// The bundled parts, or the files `SCRUTATE_SEMANTIC` and `SCRUTATE_RANKER` name (`none`
    /// turns a part off). Nothing is read here.
    pub fn from_env() -> Arc<ModelSet> {
        Arc::new(ModelSet { semantic: Source::from_var("SCRUTATE_SEMANTIC").map(Part::new), ranker: Source::from_var("SCRUTATE_RANKER").map(Part::new) })
    }

    /// No vector model and no ranker: lexical search and the linear blend.
    pub fn none() -> Arc<ModelSet> {
        Arc::new(ModelSet { semantic: None, ranker: None })
    }

    /// Whether the index keeps vectors, without loading the model.
    pub fn semantic_enabled(&self) -> bool {
        self.semantic.is_some()
    }

    /// The signature the index stores for its vectors (`none` without a vector model). A model
    /// named at run time is hashed once and the hash
    /// kept in `cache_dir` beside the file's size and times.
    pub fn signature(&self, cache_dir: &Path) -> Result<String> {
        match self.semantic.as_ref().map(|p| &p.source) {
            None => Ok("none".into()),
            Some(Source::Bundled) => Ok(BUNDLED_SIGNATURE.into()),
            Some(Source::File(path)) => cached_signature(path, &cache_dir.join("model.sig")),
        }
    }

    /// The vector model, loaded on the first call.
    pub fn semantic(&self) -> Result<Option<&SemanticModel>> {
        let Some(part) = &self.semantic else { return Ok(None) };
        part.get(|s| match s {
            Source::Bundled => SemanticModel::from_bytes(SEMANTIC_NPZ),
            Source::File(p) => SemanticModel::load(p),
        })
        .map(Some)
    }

    /// The ranker, loaded and checked against `FEATURES` on the first call.
    pub fn ranker(&self) -> Result<Option<&TreeEnsemble>> {
        let Some(part) = &self.ranker else { return Ok(None) };
        part.get(|s| {
            let (r, name) = match s {
                Source::Bundled => (TreeEnsemble::from_bytes(RANKER_NPZ)?, "the bundled ranker".to_string()),
                Source::File(p) => (TreeEnsemble::load(p)?, format!("ranker {}", p.display())),
            };
            fits(&r.feature_names).map_err(|why| anyhow!("{name} does not fit this scrutate: {why}; set SCRUTATE_RANKER=none to rank without it"))?;
            Ok(r)
        })
        .map(Some)
    }
}

/// Why a ranker fitted on `names` cannot score this engine's feature rows.
fn fits(names: &[String]) -> std::result::Result<(), String> {
    if let Some(i) = (0..names.len().min(FEATURES.len())).find(|&i| names[i] != FEATURES[i]) {
        return Err(format!("its feature {} is \"{}\" where this scrutate computes \"{}\"", i + 1, names[i], FEATURES[i]));
    }
    if names.len() != FEATURES.len() {
        return Err(format!("it reads {} features and this scrutate computes {}", names.len(), FEATURES.len()));
    }
    Ok(())
}

/// The signature of a model file: a hash of its bytes.
pub fn file_signature(bytes: &[u8]) -> String {
    use blake2::Digest;
    let out = blake2::Blake2b::<blake2::digest::consts::U16>::digest(bytes);
    format!("blake2b:{}", out.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// The signature of the model file at `path`, from `cache` when its path, size and times match.
fn cached_signature(path: &Path, cache: &Path) -> Result<String> {
    let full = std::fs::canonicalize(path).map_err(|e| anyhow!("load semantic model {}: {e}", path.display()))?;
    let stat = crate::index::Stat::of(&full, &std::fs::metadata(&full)?);
    let key = format!("{}\t{}\t{}\t{}", full.display(), stat.size, stat.mtime, stat.ctime);
    if let Some(sig) = std::fs::read_to_string(cache).ok().and_then(|t| t.strip_prefix(&format!("{key}\t")).map(|s| s.trim_end().to_string())) {
        return Ok(sig);
    }
    let sig = file_signature(&std::fs::read(&full).map_err(|e| anyhow!("load semantic model {}: {e}", path.display()))?);
    let _ = std::fs::write(cache, format!("{key}\t{sig}\n")); // a read-only index directory hashes again next time
    Ok(sig)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_signature_is_the_hash_of_the_bundled_model() {
        assert_eq!(BUNDLED_SIGNATURE, file_signature(SEMANTIC_NPZ));
    }

    #[test]
    fn the_bundled_models_fit_a_crates_io_crate_and_load() {
        assert!(SEMANTIC_NPZ.len() + RANKER_NPZ.len() < 9_000_000, "crates.io refuses a crate over 10 MB");
        let m = SemanticModel::from_bytes(SEMANTIC_NPZ).unwrap();
        assert!(m.encode_query(&[m.va.keys().next().unwrap().clone()]).is_some());
    }

    #[test]
    fn the_bundled_ranker_fits() {
        assert!(fits(&TreeEnsemble::from_bytes(RANKER_NPZ).unwrap().feature_names).is_ok());
    }

    #[test]
    fn a_ranker_with_other_features_says_which() {
        let mut names: Vec<String> = FEATURES.iter().map(|s| s.to_string()).collect();
        names[0] = "Zex_log".into();
        assert_eq!(fits(&names).unwrap_err(), "its feature 1 is \"Zex_log\" where this scrutate computes \"lex_log\"");
        names[0] = "lex_log".into();
        names.pop();
        assert!(fits(&names).unwrap_err().contains("47 features"));
    }

    #[test]
    fn a_model_file_is_hashed_once_and_again_when_it_changes() {
        let dir = std::env::temp_dir().join(format!("scrutate_model_sig_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (model, cache) = (dir.join("m.npz"), dir.join("model.sig"));
        std::fs::write(&model, b"one").unwrap();
        assert_eq!(cached_signature(&model, &cache).unwrap(), file_signature(b"one"));
        assert!(std::fs::read_to_string(&cache).unwrap().ends_with(&format!("{}\n", file_signature(b"one"))));
        std::fs::write(&model, b"two!").unwrap();
        assert_eq!(cached_signature(&model, &cache).unwrap(), file_signature(b"two!"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
