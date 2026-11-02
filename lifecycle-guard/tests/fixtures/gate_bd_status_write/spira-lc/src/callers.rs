pub fn close(bd: &mut dyn Bd, id: &str, reason: &str) -> Result<(), String> {
    record_the_end(id)?;
    bd.close(id, reason)
}
