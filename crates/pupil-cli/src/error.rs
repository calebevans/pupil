use std::path::PathBuf;

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum CliError {
    #[error("pupil.yaml not found at {path}")]
    #[diagnostic(
        code(pupil::E001),
        help("Run `pupil create <name>` to scaffold a new agent directory,\nor run this command from inside an existing agent directory.")
    )]
    ConfigNotFound { path: PathBuf },

    #[error("invalid configuration: {message}")]
    #[diagnostic(
        code(pupil::E001),
        help("Check the pupil.yaml schema reference at https://github.com/calebevans/pupil")
    )]
    ConfigInvalid { message: String },

    #[error("invalid global configuration: {message}")]
    #[diagnostic(
        code(pupil::E001),
        help("Delete ~/.config/pupil/config.yaml and run any command to re-run the setup wizard.")
    )]
    GlobalConfigInvalid { message: String },

    #[error("container runtime not found")]
    #[diagnostic(
        code(pupil::E002),
        help(
            "Install Docker (https://docs.docker.com/get-docker/)\n\
             or Podman (https://podman.io/getting-started/installation)\n\
             or set PUPIL_CONTAINER_RUNTIME to the path of a compatible runtime."
        )
    )]
    ContainerRuntimeNotFound,

    #[error("container runtime '{runtime}' is not responding")]
    #[diagnostic(
        code(pupil::E002),
        help("Start the container runtime:\n  Docker: `sudo systemctl start docker` or open Docker Desktop\n  Podman: `podman machine start`")
    )]
    ContainerRuntimeNotRunning { runtime: String },

    #[error("required environment variable '{name}' is not set")]
    #[diagnostic(
        code(pupil::E002),
        help("Set the variable in your shell:\n  export {name}=your-key-here\nor add it to your shell profile (~/.zshrc, ~/.bashrc).")
    )]
    EnvVarMissing { name: String },

    #[error("Ollama is not reachable at {url}")]
    #[diagnostic(
        code(pupil::E002),
        help("Ollama is needed for embedding generation.\nInstall from https://ollama.com and run `ollama serve`,\nor use `pupil build --with-ollama` to start the Ollama sidecar automatically.")
    )]
    OllamaNotReachable { url: String },

    #[error("build container failed to start: {message}")]
    #[diagnostic(code(pupil::E003))]
    BuildContainerFailed { message: String },

    #[error("learning failed (exit code {exit_code}): {stderr}")]
    #[diagnostic(
        code(pupil::E003),
        help("Check the learning agent output above for details.\nCommon causes: invalid API key, network issues, or malformed curriculum files.")
    )]
    LearningFailed { exit_code: i32, stderr: String },

    #[error("image commit failed: {message}")]
    #[diagnostic(code(pupil::E003))]
    CommitFailed { message: String },

    #[error("no curriculum sources found in {path}")]
    #[diagnostic(
        code(pupil::E003),
        help("Add curriculum files with `pupil teach <name> <paths...>`\nor add sources to the `curriculum.sources` section in pupil.yaml.")
    )]
    NoCurriculumSources { path: PathBuf },

    #[error("agent container failed to start: {message}")]
    #[diagnostic(code(pupil::E004))]
    RunContainerFailed { message: String },

    #[error("agent container exited unexpectedly (exit code {exit_code})")]
    #[diagnostic(
        code(pupil::E004),
        help("Run `pupil logs <name>` to see the agent container output.")
    )]
    AgentCrashed { exit_code: i32 },

    #[error("no image found for agent '{name}'")]
    #[diagnostic(
        code(pupil::E004),
        help("Build the agent first with `pupil build {name}`.")
    )]
    ImageNotFound { name: String },

    #[error("registry authentication failed for {registry}: {message}")]
    #[diagnostic(
        code(pupil::E005),
        help("Check your registry credentials.\nFor GHCR: `echo $GITHUB_TOKEN | docker login ghcr.io -u USERNAME --password-stdin`")
    )]
    RegistryAuthFailed { registry: String, message: String },

    #[error("push to {reference} failed: {message}")]
    #[diagnostic(code(pupil::E005))]
    PushFailed { reference: String, message: String },

    #[error("pull from {reference} failed: {message}")]
    #[diagnostic(code(pupil::E005))]
    PullFailed { reference: String, message: String },

    #[error("LLM error ({provider}): {message}")]
    #[diagnostic(code(pupil::E006))]
    LlmError { provider: String, message: String },

    #[error("{0}")]
    #[diagnostic(code(pupil::internal))]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    #[diagnostic(code(pupil::internal))]
    Json(#[from] serde_json::Error),

    #[error("YAML error: {0}")]
    #[diagnostic(code(pupil::internal))]
    Yaml(#[from] serde_yml::Error),

    #[error("container runtime error: {message}")]
    #[diagnostic(code(pupil::internal))]
    ContainerRuntimeError { message: String },

    #[error("{0}")]
    #[diagnostic(code(pupil::internal))]
    Container(#[from] crate::container::ContainerError),
}

pub mod exit_code {
    pub const OK: i32 = 0;
    pub const FAILURE: i32 = 1;
    #[allow(dead_code)]
    pub const TEST_FAILURE: i32 = 1;
    #[allow(dead_code)]
    pub const TEST_ERROR: i32 = 2;
}
