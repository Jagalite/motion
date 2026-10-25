//! Exact application-owned read routes, not a native catalog or mutation grant.
//! Every selected handler reauthenticates SystemAdmin in its read transaction.

pub(super) const AUTH_ROUTES: &[&str] = &[
    "/admin/auth/overview",
    "/admin/auth/restore-review",
    "/admin/auth/providers",
    "/admin/auth/email",
    "/admin/auth/email/activity",
    "/admin/auth/registration",
    "/admin/auth/requests",
    "/admin/auth/fresh-approvals",
    "/admin/auth/identities",
    "/admin/auth/sessions",
    "/admin/auth/audit",
];

pub(super) fn supported(method: &str, path: &str) -> bool {
    method == "GET"
        && (path == "/admin/diagnostics"
            || AUTH_ROUTES.contains(&path)
            || path
                .strip_prefix("/admin/auth/users/")
                .is_some_and(super::catalog::valid_id))
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_admin_reads_are_exact_gets_not_an_admin_prefix() {
        for path in super::AUTH_ROUTES
            .iter()
            .copied()
            .chain(["/admin/diagnostics", "/admin/auth/users/user-1"])
        {
            assert!(super::supported("GET", path), "{path}");
            for method in ["HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
                assert!(!super::supported(method, path), "{method} {path}");
            }
            for suffix in ["/", "/extra", "/approve", "/revoke", "/test"] {
                assert!(!super::supported("GET", &format!("{path}{suffix}")));
            }
        }
        for path in [
            "/admin",
            "/admin/auth",
            "/admin/auth/settings",
            "/admin/auth/providers/provider-1",
            "/admin/auth/email/diagnostics",
            "/admin/auth/email/tests/test-1/confirm",
            "/admin/auth/users/",
            "/admin/auth/users/../overview",
            "/admin/auth/users/user%2Fother",
            "/admin/backups",
            "/admin/backups/backup-1",
            "/admin/cleanup",
            "/admin/restore",
            "/admin/install",
            "/admin/deploy",
        ] {
            assert!(!super::supported("GET", path), "{path}");
        }
    }
}
