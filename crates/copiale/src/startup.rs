//! Start at login under MSIX: the manifest's startup task takes no arguments.

use windows::ApplicationModel::Activation::ActivationKind;
use windows::ApplicationModel::AppInstance;

/// Whether Windows started this process for the startup task; never when
/// unpackaged.
pub fn at_login() -> bool {
    AppInstance::GetActivatedEventArgs()
        .and_then(|args| args.Kind())
        .is_ok_and(|kind| kind == ActivationKind::StartupTask)
}
