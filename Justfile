bin_dir := env("HOME") / ".local/bin"
data_dir := env("HOME") / ".local/share"

# List the recipes
default:
    @just --list

# Run the editor. Pass an image or a .pixel project to open it
[positional-arguments]
run *args:
    cargo run --release -- "$@"

# Run the tests
test:
    cargo test

# Build a release binary and install it with its desktop entry, icon, and file type
install:
    cargo build --release
    install -Dm755 target/release/pixel {{bin_dir}}/pixel
    install -Dm644 assets/pixel.desktop {{data_dir}}/applications/pixel.desktop
    install -Dm644 assets/pixel.svg {{data_dir}}/icons/hicolor/scalable/apps/pixel.svg
    install -Dm644 assets/pixel.xml {{data_dir}}/mime/packages/pixel.xml
    update-desktop-database {{data_dir}}/applications
    update-mime-database {{data_dir}}/mime

# Remove everything install put in place
uninstall:
    rm -f {{bin_dir}}/pixel
    rm -f {{data_dir}}/applications/pixel.desktop
    rm -f {{data_dir}}/icons/hicolor/scalable/apps/pixel.svg
    rm -f {{data_dir}}/mime/packages/pixel.xml
    update-desktop-database {{data_dir}}/applications
    update-mime-database {{data_dir}}/mime
