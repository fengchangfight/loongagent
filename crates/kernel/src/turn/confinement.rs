use super::*;

impl Confinement for LandlockConfinement {
    fn confine(&self, command: &mut Command) {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let policy = self.policy.clone();
            // SAFETY: `apply` is documented to do no allocation on its success
            // path, which is what makes it safe between fork and exec.
            unsafe {
                command.pre_exec(move || {
                    policy.apply().map_err(|e| {
                        std::io::Error::new(std::io::ErrorKind::PermissionDenied, e.to_string())
                    })
                });
            }
        }
        #[cfg(not(unix))]
        let _ = command;
    }
}
impl Kernel {
    pub fn pdp_mut(&mut self) -> &mut Pdp {
        &mut self.pdp
    }

    /// What confinement is in force for spawned processes.
    pub fn confinement_note(&self) -> &str {
        &self.confinement_note
    }

    /// Replace the confinement policy applied to every process a tool spawns.
    pub fn set_confinement(&mut self, confinement: Box<dyn Confinement>) {
        self.confinement = confinement;
    }
}
