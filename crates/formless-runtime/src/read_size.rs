//! Source-aware whole-read size diagnostics; byte counts are authoritative.

#[derive(Clone, Copy)]
pub(super) enum ReadSize {
    Reported(u64),
    AtLeast(u64),
    Exact(u64),
}

impl ReadSize {
    pub(super) fn bytes(self) -> u64 {
        match self {
            Self::Reported(bytes) | Self::AtLeast(bytes) | Self::Exact(bytes) => bytes,
        }
    }

    pub(super) fn over_limit(self, what: &str, cap: u64, setting: &str, instead: &str) -> String {
        let description = match self {
            Self::Reported(bytes) => format!("reported size {bytes} bytes"),
            Self::AtLeast(bytes) => format!("observed at least {bytes} bytes"),
            Self::Exact(bytes) => format!("observed {bytes} bytes"),
        };
        format!("{what}: {description}, over the {cap}-byte resource limit ({setting}); {instead}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_bytes_distinguish_cap_plus_one_small_and_large_limits() {
        for (size, cap) in [(67_108_865, 67_108_864), (2, 1), (u64::MAX, u64::MAX - 1)] {
            let error = ReadSize::AtLeast(size).over_limit(
                "fetch",
                cap,
                "tools.max_read_bytes",
                "stream it with fetchChunks",
            );
            assert!(
                error.contains(&format!("observed at least {size} bytes")),
                "{error}"
            );
            assert!(
                error.contains(&format!("{cap}-byte resource limit")),
                "{error}"
            );
            assert!(error.contains("tools.max_read_bytes") && error.contains("fetchChunks"));
            assert!(!error.contains("MB"));
        }
    }

    #[test]
    fn reported_sizes_and_complete_buffers_keep_their_provenance() {
        let reported = ReadSize::Reported(1001).over_limit(
            "fs.read",
            1000,
            "tools.max_read_bytes",
            "use fs.readChunks or fs.readLines",
        );
        assert!(reported.contains("reported size 1001 bytes"));
        let exact = ReadSize::Exact(1001).over_limit(
            "fs.read",
            1000,
            "tools.max_read_bytes",
            "use fs.readChunks or fs.readLines",
        );
        assert!(exact.contains("observed 1001 bytes") && !exact.contains("at least"));
        assert_eq!(ReadSize::Reported(1001).bytes(), 1001);
        assert_eq!(ReadSize::AtLeast(1001).bytes(), 1001);
        assert_eq!(ReadSize::Exact(1001).bytes(), 1001);
        for error in [reported, exact] {
            assert!(
                error.contains("fs.read")
                    && error.contains("tools.max_read_bytes")
                    && error.contains("fs.readChunks")
                    && error.contains("fs.readLines")
            );
        }
    }
}
