# Vendored daisyUI build plugins

These files are the standalone Tailwind CSS plugins from daisyUI 5.7.4:

- `daisyui-5.7.4.mjs`
  (`sha256:909c10413788d75efefc5ef175cb1960c43125383bb0dcee613d95491b2339`)
- `daisyui-theme-5.7.4.mjs`
  (`sha256:5c07cd4fb5be9dba7180cf83312d1dc7e3413bb492f82381834557a772c47b2a`)

They are vendored because Cargo Leptos uses Tailwind's standalone CLI. The files
come from the corresponding [daisyUI GitHub release][release] and are licensed
under the [MIT license][license].

[release]: https://github.com/saadeghi/daisyui/releases/tag/v5.7.4
[license]: https://github.com/saadeghi/daisyui/blob/v5.7.4/LICENSE
