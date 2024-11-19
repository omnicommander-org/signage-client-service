{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
    nativeBuildInputs = with pkgs.buildPackages; [
        rustc
        cargo
        mold

        openssl
        pkg-config

        cargo-deny
        cargo-audit
        clippy
        rustfmt

		mpv
        xorg.libxcb
        scrot
    ];
}

