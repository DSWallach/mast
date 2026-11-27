//! Task classification and the learned model floors shared within a Space.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRole {
    #[default]
    Implementer,
    TestAuthor,
    DocWriter,
    Reviewer,
    LeadIntegrator,
    ArchitectPlanner,
    SpecAuditor,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskDomain {
    #[default]
    MechanicalRefactor,
    DocsConfig,
    McpOrCli,
    UiGpui,
    HarnessProtocol,
    ProtoOrDocSchema,
    EngineConcurrencyState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComplexityTier {
    Trivial,
    #[default]
    Standard,
    Complex,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TaskClass {
    pub role: TaskRole,
    pub domain: TaskDomain,
    pub complexity: ComplexityTier,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelTier {
    #[default]
    Small,
    Medium,
    Large,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ModelFitPolicy {
    pub space_id: String,
    pub task_class: TaskClass,
    pub min_tier: ModelTier,
    pub blocked_models: Vec<crate::StandingRoleModel>,
}
