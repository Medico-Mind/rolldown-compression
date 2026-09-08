//! Typed failures shared by compression, scheduling, and the N-API binding.

use napi::{Error as NapiError, Status};

use crate::compress::Algorithm;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unknown algorithm `{0}`, expected one of: gzip, brotli, zstd")]
    UnknownAlgorithm(String),
    #[error("invalid {algorithm} level {level}: expected {min}..={max}")]
    InvalidLevel {
        algorithm: Algorithm,
        level: u32,
        min: u32,
        max: u32,
    },
    #[error("invalid brotli windowBits {0}: expected 10..=24")]
    InvalidWindowBits(u32),
    #[error("invalid brotli sectionSize {0}: expected a positive number of bytes")]
    InvalidSectionSize(u32),
    #[error("tasks and buffers must have the same length (got {tasks} tasks, {buffers} buffers)")]
    BatchLengthMismatch { tasks: usize, buffers: usize },
    #[error("buffers larger than 4 GiB are not supported")]
    BufferTooLarge,
    #[error("gzip compression failed: {0}")]
    Gzip(#[source] std::io::Error),
    #[error("zstd compression failed: {0}")]
    Zstd(#[source] std::io::Error),
    #[error("{0}")]
    BrotliConfig(#[from] mbrotli::ConfigError),
    #[error("{0}")]
    Brotli(#[from] mbrotli::EncodeError),
    #[error("{0}")]
    BrotliParallelConfig(#[from] mbrotli::compressor::parallel::ParallelConfigError),
    #[error("{0}")]
    BrotliParallel(#[from] mbrotli::compressor::parallel::ParallelEncodeError),
    #[error("{0} compression panicked unexpectedly")]
    CompressionPanicked(Algorithm),
}

impl From<Error> for NapiError {
    fn from(error: Error) -> Self {
        let status = match &error {
            Error::UnknownAlgorithm(_)
            | Error::InvalidLevel { .. }
            | Error::InvalidWindowBits(_)
            | Error::InvalidSectionSize(_)
            | Error::BatchLengthMismatch { .. }
            | Error::BufferTooLarge => Status::InvalidArg,
            Error::Gzip(_)
            | Error::Zstd(_)
            | Error::BrotliConfig(_)
            | Error::Brotli(_)
            | Error::BrotliParallelConfig(_)
            | Error::BrotliParallel(_)
            | Error::CompressionPanicked(_) => Status::GenericFailure,
        };
        NapiError::new(status, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn validation_failures_convert_to_invalid_arg() {
        let errors = [
            "lzma".parse::<Algorithm>().unwrap_err(),
            Algorithm::Gzip.validate_level(10).unwrap_err(),
            crate::compress::validate_window_bits(9).unwrap_err(),
            crate::compress::validate_section_size(0).unwrap_err(),
            Error::BatchLengthMismatch {
                tasks: 1,
                buffers: 0,
            },
            Error::BufferTooLarge,
        ];
        for error in errors {
            let message = error.to_string();
            let napi_error = NapiError::from(error);
            assert_eq!(
                (napi_error.status, napi_error.reason),
                (Status::InvalidArg, message)
            );
        }
    }

    #[test]
    fn codec_failures_retain_their_typed_sources() {
        for error in [
            Error::Gzip(std::io::Error::other("encoder failed")),
            Error::Zstd(std::io::Error::other("encoder failed")),
        ] {
            let source = error
                .source()
                .unwrap()
                .downcast_ref::<std::io::Error>()
                .unwrap();
            assert_eq!(source.kind(), std::io::ErrorKind::Other);
        }
        let source = mbrotli::Quality::try_from(12).unwrap_err();
        let error = Error::from(source);
        assert!(error.source().unwrap().is::<mbrotli::ConfigError>());
    }

    #[test]
    fn runtime_failures_convert_to_generic_failure() {
        let errors = [
            Error::Gzip(std::io::Error::other("encoder failed")),
            Error::Zstd(std::io::Error::other("encoder failed")),
            Error::from(mbrotli::Quality::try_from(12).unwrap_err()),
            Error::from(mbrotli::compressor::parallel::TaskCount::try_from(0).unwrap_err()),
            Error::from(mbrotli::compressor::parallel::ParallelEncodeError::Cancelled),
            Error::CompressionPanicked(Algorithm::Brotli),
        ];
        for error in errors {
            let message = error.to_string();
            let napi_error = NapiError::from(error);
            assert_eq!(
                (napi_error.status, napi_error.reason),
                (Status::GenericFailure, message)
            );
        }
    }
}
