//! Separate default circuit groups by service and use fresh groups for retries.

use std::fmt;

use arti_client::StreamPrefs;
use arti_client::config::BoolOrAuto;
use arti_client::isolation::IsolationHelper;

use crate::OnionLocator;

#[derive(Clone, PartialEq, Eq)]
struct ServiceIsolation(OnionLocator);

impl fmt::Debug for ServiceIsolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ServiceIsolation(..)")
    }
}

impl IsolationHelper for ServiceIsolation {
    fn compatible_same_type(&self, other: &Self) -> bool {
        self == other
    }

    fn join_same_type(&self, other: &Self) -> Option<Self> {
        (self == other).then(|| self.clone())
    }
}

pub(super) fn preferences(locator: &OnionLocator, fresh: bool) -> StreamPrefs {
    let mut prefs = StreamPrefs::new();
    prefs.connect_to_onion_services(BoolOrAuto::Explicit(true));
    if fresh {
        // Retain Arti's guard state while bypassing the previous circuit group.
        prefs.new_isolation_group();
    } else {
        prefs.set_isolation(ServiceIsolation(locator.clone()));
    }
    prefs
}
