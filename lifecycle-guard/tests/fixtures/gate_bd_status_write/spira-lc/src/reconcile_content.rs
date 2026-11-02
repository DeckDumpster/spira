pub fn reconcile(bd: &mut dyn Bd, id: &str, reason: &str) -> Result<(), String> {
    bd.close(id, reason)
}
