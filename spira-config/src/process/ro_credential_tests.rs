    use super::ro_credential;

    #[test]
    fn the_read_only_secret_is_the_service_secrets_ro_sibling_and_never_a_guess() {
        assert_eq!(ro_credential("/c/spira-lc.credential"), "/c/spira-lc.credential-ro");
        assert_eq!(ro_credential(" "), "", "no service credential names no read-only one");
    }
