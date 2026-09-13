//! Native component integration code, independent of the component's implementation language.
#[cfg(feature = "runtime")]
pub mod authentication;
#[cfg(all(unix, feature = "runtime"))]
pub mod cln;
#[cfg(feature = "runtime")]
pub mod coco;
#[cfg(feature = "runtime")]
pub mod http;
#[cfg(feature = "runtime")]
pub mod inspect;
#[cfg(feature = "runtime")]
pub mod management;
#[cfg(feature = "runtime")]
pub mod nutshell;
#[cfg(feature = "runtime")]
pub mod quote;
#[cfg(feature = "observation")]
pub mod wallet;

pub const BINARY: &str = "/opt/proofstorm/driver";
pub const VERSION: u32 = 1;
