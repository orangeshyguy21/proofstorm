//! Fixed helper images used outside component catalog entries.
pub const PROBE_IMAGE: &str = "proofstorm-registry.localhost:5000/upstream/docker.io/library/busybox@sha256:73aaf090f3d85aa34ee199857f03fa3a95c8ede2ffd4cc2cdb5b94e566b11662";
// Candidate-build helpers pull from Proofstorm's GHCR mirror of the pinned
// upstream images; the origins below record where those digests came from.
pub const GIT_IMAGE: &str = "ghcr.io/osygy/proofstorm/upstream@sha256:c0280cf9572316299b08544065d3bf35db65043d5e3963982ec50647d2746e26";
pub const BUILDKIT_IMAGE: &str = "ghcr.io/osygy/proofstorm/upstream@sha256:a02f6571999693089dc928e9bbb64836c21703b214195d8637c011f1a7025ef6";
pub const HELPER_IMAGES: [&str; 3] = [PROBE_IMAGE, GIT_IMAGE, BUILDKIT_IMAGE];

#[cfg(test)]
mod tests {
    #[test]
    fn mirrored_helpers_keep_their_origin_digests() {
        for (image, origin) in [super::GIT_IMAGE, super::BUILDKIT_IMAGE]
            .into_iter()
            .zip(proofstorm_core::UPSTREAM_HELPER_ORIGINS)
        {
            assert!(image.starts_with(proofstorm_core::UPSTREAM_MIRROR));
            assert_eq!(
                image.split_once('@').unwrap().1,
                origin.split_once('@').unwrap().1
            );
        }
    }
}
