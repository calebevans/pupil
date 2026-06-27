//! Test schema types shared between the CLI and the in-container test runner.
//!
//! These mirror the types in `pupil-cli/src/commands/test.rs` so the
//! in-container runner can parse the same test YAML format.

use std::collections::HashSet;
use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Test YAML schema types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TestFile {
    #[serde(default)]
    pub config: TestConfig,
    pub tests: Vec<TestCaseDef>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TestConfig {
    #[serde(default)]
    pub temperature: f64,
    #[serde(default)]
    pub retries: u32,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub judge_model: Option<String>,
    #[serde(default = "default_threshold")]
    pub threshold: f64,
}

fn default_timeout_secs() -> u64 {
    30
}

fn default_threshold() -> f64 {
    0.8
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            temperature: 0.0,
            retries: 0,
            timeout_secs: default_timeout_secs(),
            judge_model: None,
            threshold: default_threshold(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TestCaseDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub question: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub threshold: Option<f64>,
    pub expects: Vec<Assertion>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Assertion {
    Contains(String),
    NotContains(String),
    ContainsAny(Vec<String>),
    ContainsAll(Vec<String>),
    Matches(String),
    NotMatches(String),
    StartsWith(String),
    LlmJudge(LlmJudgeConfig),
    Other(String),
}

impl<'de> Deserialize<'de> for Assertion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct AssertionVisitor;

        impl<'de> Visitor<'de> for AssertionVisitor {
            type Value = Assertion;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a single-key map representing an assertion")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Assertion, M::Error>
            where
                M: MapAccess<'de>,
            {
                let key: String =
                    map.next_key()?.ok_or_else(|| de::Error::custom("empty assertion map"))?;

                let assertion = match key.as_str() {
                    "contains" => Assertion::Contains(map.next_value()?),
                    "not_contains" => Assertion::NotContains(map.next_value()?),
                    "contains_any" => Assertion::ContainsAny(map.next_value()?),
                    "contains_all" => Assertion::ContainsAll(map.next_value()?),
                    "matches" => Assertion::Matches(map.next_value()?),
                    "not_matches" => Assertion::NotMatches(map.next_value()?),
                    "starts_with" => Assertion::StartsWith(map.next_value()?),
                    "llm_judge" => Assertion::LlmJudge(map.next_value()?),
                    other => {
                        // Consume the value to advance the deserializer
                        let _: serde::de::IgnoredAny = map.next_value()?;
                        Assertion::Other(other.to_string())
                    }
                };

                // Consume any remaining keys
                while map.next_key::<String>()?.is_some() {
                    let _: serde::de::IgnoredAny = map.next_value()?;
                }

                Ok(assertion)
            }
        }

        deserializer.deserialize_map(AssertionVisitor)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LlmJudgeConfig {
    pub criteria: String,
    #[serde(default)]
    pub threshold: Option<f64>,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

impl TestFile {
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if self.tests.is_empty() {
            errors.push("tests list must not be empty".to_string());
        }

        let mut seen_names = HashSet::new();
        for test in &self.tests {
            if !seen_names.insert(&test.name) {
                errors.push(format!("duplicate test name: '{}'", test.name));
            }
        }

        if !(0.0..=1.0).contains(&self.config.threshold) {
            errors.push(format!(
                "config.threshold must be 0.0-1.0, got {}",
                self.config.threshold
            ));
        }

        if self.config.timeout_secs == 0 {
            errors.push("config.timeout_secs must be > 0".to_string());
        }

        let name_re = regex::Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9_-]*$").unwrap();

        for test in &self.tests {
            let prefix = format!("test '{}': ", test.name);

            if !name_re.is_match(&test.name) {
                errors.push(format!(
                    "{prefix}name must start with alphanumeric and contain only \
                     alphanumeric, hyphens, underscores"
                ));
            }

            if test.question.trim().is_empty() {
                errors.push(format!("{prefix}question must not be empty"));
            }

            if test.expects.is_empty() {
                errors.push(format!("{prefix}expects must have at least one assertion"));
            }

            if let Some(t) = test.threshold {
                if !(0.0..=1.0).contains(&t) {
                    errors.push(format!("{prefix}threshold must be 0.0-1.0, got {t}"));
                }
            }

            for (i, assertion) in test.expects.iter().enumerate() {
                let a_prefix = format!("{prefix}expects[{i}]: ");
                match assertion {
                    Assertion::Matches(pattern) | Assertion::NotMatches(pattern) => {
                        if let Err(e) = regex::Regex::new(pattern) {
                            errors.push(format!("{a_prefix}invalid regex: {e}"));
                        }
                    }
                    Assertion::ContainsAny(list) if list.is_empty() => {
                        errors.push(format!("{a_prefix}contains_any list must not be empty"));
                    }
                    Assertion::ContainsAll(list) if list.is_empty() => {
                        errors.push(format!("{a_prefix}contains_all list must not be empty"));
                    }
                    Assertion::LlmJudge(cfg) if cfg.criteria.trim().is_empty() => {
                        errors.push(format!("{a_prefix}llm_judge.criteria must not be empty"));
                    }
                    Assertion::LlmJudge(cfg) => {
                        if let Some(t) = cfg.threshold {
                            if !(0.0..=1.0).contains(&t) {
                                errors.push(format!(
                                    "{a_prefix}llm_judge.threshold must be 0.0-1.0, got {t}"
                                ));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// Return a string name for the assertion type.
pub fn assertion_type_name(assertion: &Assertion) -> String {
    match assertion {
        Assertion::Contains(_) => "contains",
        Assertion::NotContains(_) => "not_contains",
        Assertion::ContainsAny(_) => "contains_any",
        Assertion::ContainsAll(_) => "contains_all",
        Assertion::Matches(_) => "matches",
        Assertion::NotMatches(_) => "not_matches",
        Assertion::StartsWith(_) => "starts_with",
        Assertion::LlmJudge(_) => "llm_judge",
        Assertion::Other(name) => name.as_str(),
    }
    .to_string()
}
