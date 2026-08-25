use thiserror::Error;

use super::domain::MemoryContractError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MemoryWriterErrorKind {
    DigestKeyUnavailable,
    InvalidProposal,
    PolicyRejected,
    SourceRejected,
    SourceLimitExceeded,
    EvidenceMismatch,
    UnknownDigestKey,
    CorruptHistory,
    CorruptProjection,
    ProjectionStale,
    StorageFailure,
    ConflictExhausted,
}

impl MemoryWriterErrorKind {
    pub(crate) const fn stable_code(self) -> &'static str {
        match self {
            Self::DigestKeyUnavailable => "LBR-MEMORY-001",
            Self::InvalidProposal | Self::SourceLimitExceeded => "LBR-MEMORY-002",
            Self::PolicyRejected | Self::SourceRejected | Self::UnknownDigestKey => {
                "LBR-MEMORY-003"
            }
            Self::CorruptHistory | Self::CorruptProjection | Self::EvidenceMismatch => {
                "LBR-MEMORY-004"
            }
            Self::ProjectionStale => "LBR-MEMORY-PROJECTION-STALE",
            Self::StorageFailure | Self::ConflictExhausted => "LBR-MEMORY-005",
        }
    }
}

#[derive(Clone, Debug, Error)]
#[error("{code}: {summary}", code = .kind.stable_code())]
pub(crate) struct MemoryWriterError {
    kind: MemoryWriterErrorKind,
    summary: String,
}

impl MemoryWriterError {
    pub(crate) fn new(kind: MemoryWriterErrorKind, summary: impl Into<String>) -> Self {
        Self {
            kind,
            summary: summary.into(),
        }
    }

    pub(crate) const fn kind(&self) -> MemoryWriterErrorKind {
        self.kind
    }

    pub(crate) const fn stable_code(&self) -> &'static str {
        self.kind.stable_code()
    }
}

impl From<MemoryContractError> for MemoryWriterError {
    fn from(error: MemoryContractError) -> Self {
        Self::new(MemoryWriterErrorKind::InvalidProposal, error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_error_categories_have_stable_redacted_messages() {
        for (kind, code) in [
            (MemoryWriterErrorKind::SourceLimitExceeded, "LBR-MEMORY-002"),
            (MemoryWriterErrorKind::SourceRejected, "LBR-MEMORY-003"),
            (MemoryWriterErrorKind::EvidenceMismatch, "LBR-MEMORY-004"),
        ] {
            let error = MemoryWriterError::new(kind, "source validation failed");
            assert_eq!(error.stable_code(), code);
            assert_eq!(
                error.to_string(),
                format!("{code}: source validation failed")
            );
        }
    }
}
