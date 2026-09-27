mod io;

fn run(env: &io::Env) {
    io::land_mark(env, "sp-1", "RED", "deadbeef", "conflicts-with-base");
}
