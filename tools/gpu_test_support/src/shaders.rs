//! The test shaders: Marlin shaders compiled at run time, the first time a test asks for one.
//!
//! One compiler session per test binary holds the shared package `Test` (this crate's
//! `shaders/`) and, for a test that asks with [`Shader::in_crate`] or [`crate_shader!`], the
//! package of its own crate (its `tests/data/shaders/`), which may use the shared one. Every
//! result is kept for the binary's life, like [`crate::devices`].
//!
//! The compiler writes SPIR-V (the `vulkan-1.4` target) with the Development profile; there are
//! no test shaders for a backend that takes another format.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use marlin_rhi::{SHADER_FORMAT, ShaderFormat, ShaderModule};
use marlin_shaderc::{
    ConstValue, EntryRequest, PackageDesc, Profile, Session, SessionDesc, ShaderReflection,
    SourceError, SourcePath, SourceProvider,
};

/// The shared package and namespace: an entry named without a namespace is in it.
pub const SHARED: &str = "Test";

const TARGET: &str = "vulkan-1.4";

/// One test shader: an entry point and the static parameters set for it.
#[derive(Clone, Debug)]
pub struct Shader<'a> {
    entry: &'a str,
    crate_dir: Option<&'a str>,
    consts: Vec<(&'a str, ConstValue)>,
}

impl<'a> Shader<'a> {
    /// Entry `entry`: `Fill` for `Test.Fill` of the shared package, or a full path
    /// (`CookTexture.TexelLoad`).
    pub fn new(entry: &'a str) -> Self {
        Self {
            entry,
            crate_dir: None,
            consts: Vec::new(),
        }
    }

    /// Also compiles the package of the crate at `manifest_dir` (`env!("CARGO_MANIFEST_DIR")`):
    /// the `.mshader` files in its `tests/data/shaders/`.
    pub fn in_crate(mut self, manifest_dir: &'a str) -> Self {
        self.crate_dir = Some(manifest_dir);
        self
    }

    /// Sets static parameter `path` (`Test.Reduce.Width`, 0004 §13.3) for this compile.
    pub fn with_const(mut self, path: &'a str, value: ConstValue) -> Self {
        self.consts.push((path, value));
        self
    }

    /// The compiled entry point, for the backend's [`SHADER_FORMAT`]. Panics with the
    /// compiler's diagnostics if it does not compile.
    pub fn module(&self) -> ShaderModule<'static> {
        ShaderModule {
            code: self.compiled().code,
            entry: "main",
        }
    }

    /// What the compiler says about the entry point (its stage, group size, push layout...).
    pub fn reflection(&self) -> &'static ShaderReflection {
        &self.compiled().reflection
    }

    fn compiled(&self) -> &'static Compiled {
        match SHADER_FORMAT {
            ShaderFormat::SpirV => {}
            format => panic!(
                "no test shaders for {format:?}: the Marlin shader compiler writes SPIR-V only"
            ),
        }
        let entry = if self.entry.contains('.') {
            self.entry.to_string()
        } else {
            format!("{SHARED}.{}", self.entry)
        };
        let key = format!("{entry} {:?} {:?}", self.crate_dir, self.consts);
        let mut compiler = compiler().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&c) = compiler.cache.get(&key) {
            return c;
        }
        if let Some(dir) = self.crate_dir {
            compiler.add_crate(dir);
        }
        let request = EntryRequest {
            entry: &entry,
            type_args: &[],
            consts: &self.consts,
            target: TARGET,
            profile: Profile::Development,
        };
        let out = match compiler.session.compile(&request) {
            Ok(out) => out,
            Err(e) => {
                let text = compiler.session.render(&e.diagnostics);
                drop(compiler);
                panic!("test shader `{entry}` does not compile:\n{text}");
            }
        };
        let compiled: &'static Compiled = Box::leak(Box::new(Compiled {
            code: Box::leak(out.code.bytes.into_boxed_slice()),
            reflection: out.reflection,
        }));
        compiler.cache.insert(key, compiled);
        compiled
    }
}

/// The compiled entry point `entry` of the shared package (`Fill`) or by its full path. Panics
/// if it does not compile.
pub fn shader(entry: &str) -> ShaderModule<'static> {
    Shader::new(entry).module()
}

/// The compiled entry point `entry` (a full path) of the calling crate's own package
/// (`tests/data/shaders/`), which may use the shared one.
#[macro_export]
macro_rules! crate_shader {
    ($entry:expr) => {
        $crate::Shader::new($entry)
            .in_crate(env!("CARGO_MANIFEST_DIR"))
            .module()
    };
}

struct Compiled {
    code: &'static [u8],
    reflection: ShaderReflection,
}

struct Compiler {
    session: Session<'static>,
    sources: &'static Dirs,
    /// The crates whose packages are in the session, by manifest directory.
    crates: Vec<String>,
    cache: HashMap<String, &'static Compiled>,
}

impl Compiler {
    fn add_crate(&mut self, manifest_dir: &str) {
        if self.crates.iter().any(|d| d == manifest_dir) {
            return;
        }
        let name = Path::new(manifest_dir)
            .file_name()
            .and_then(|n| n.to_str())
            .expect("a crate directory")
            .to_string();
        let dir = marlin_test_support::data_path(manifest_dir, "shaders");
        self.sources.add(&name, dir);
        self.session
            .add_package(&PackageDesc {
                name: &name,
                dependencies: &["Marlin", SHARED],
            })
            .unwrap_or_else(|e| panic!("the test shaders of {manifest_dir}: {e}"));
        self.crates.push(manifest_dir.to_string());
    }
}

fn compiler() -> &'static Mutex<Compiler> {
    static COMPILER: OnceLock<Mutex<Compiler>> = OnceLock::new();
    COMPILER.get_or_init(|| {
        let sources: &'static Dirs = Box::leak(Box::new(Dirs::default()));
        sources.add(
            SHARED,
            Path::new(env!("CARGO_MANIFEST_DIR")).join("shaders"),
        );
        let targets = Box::leak(Box::new(marlin_shaderc::default_targets()));
        let mut session = Session::new(SessionDesc { sources, targets });
        session
            .add_package(&PackageDesc {
                name: SHARED,
                dependencies: &["Marlin"],
            })
            .unwrap_or_else(|e| panic!("the shared test shaders: {e}"));
        Mutex::new(Compiler {
            session,
            sources,
            crates: Vec::new(),
            cache: HashMap::new(),
        })
    })
}

/// Each package's directory: its `.mshader` files, by name.
#[derive(Default)]
struct Dirs(Mutex<HashMap<String, PathBuf>>);

impl Dirs {
    fn add(&self, package: &str, dir: PathBuf) {
        self.0.lock().unwrap().insert(package.to_string(), dir);
    }

    fn dir(&self, package: &str) -> Result<PathBuf, SourceError> {
        self.0
            .lock()
            .unwrap()
            .get(package)
            .cloned()
            .ok_or_else(|| SourceError {
                message: format!("no test shader package `{package}`"),
            })
    }
}

impl SourceProvider for Dirs {
    fn list(&self, package: &str) -> Result<Vec<SourcePath>, SourceError> {
        let dir = self.dir(package)?;
        let error = |e: std::io::Error| SourceError {
            message: format!("{}: {e}", dir.display()),
        };
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(error)? {
            let name = entry.map_err(error)?.file_name();
            if let Some(name) = name.to_str().filter(|n| n.ends_with(".mshader")) {
                names.push(name.to_string());
            }
        }
        names.sort();
        Ok(names
            .into_iter()
            .map(|n| SourcePath::new(package, n.as_str()))
            .collect())
    }

    fn read(&self, path: &SourcePath) -> Result<Arc<str>, SourceError> {
        let file = self.dir(path.package())?.join(path.path());
        // Line endings as `\n`, so diagnostics and hashes are the same on every checkout.
        std::fs::read_to_string(&file)
            .map(|text| text.replace("\r\n", "\n").into())
            .map_err(|e| SourceError {
                message: format!("{}: {e}", file.display()),
            })
    }
}
