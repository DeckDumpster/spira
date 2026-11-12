    use super::*;

    #[test]
    fn same_user_mode_defaults_to_the_user_units_socket() {
        assert_eq!(lc_socket_default(false, 1000), "/run/user/1000/spira-lc/sock");
        assert_eq!(lc_socket_default(false, 0), "/run/user/0/spira-lc/sock");
    }

    #[test]
    fn system_mode_defaults_to_the_system_socket() {
        assert_eq!(lc_socket_default(true, 1000), "/run/spira-lc/sock");
        assert_eq!(lc_socket_default(true, 1000), LC_SYSTEM_SOCKET);
    }

