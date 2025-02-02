pytest = find_program('pytest-3', 'pytest', required: get_option('tests'))
pymod = import('python')
python = pymod.find_installation('python3', required: get_option('tests'))

build_tests = pytest.found() and python.found()
if not build_tests
    subdir_done()
endif

cflags = []
if cc_id == 'clang'
    cflags += [
        '-Wno-unknown-attributes', # warning: unknown attribute 'preserve_access_index' ignored [-Wunknown-attributes]
    ]
endif
add_project_arguments(cc.get_supported_arguments(cflags), language: 'c')

# We recompile the BPF as a normal shared library, one
# per BPF program.
foreach obj: bpf_objects
    srcfile = obj['file']
    outfile = obj['output_file']
    bpf_o = obj['bpf_o']
    src = ['test-wrapper.c'] + srcfile
    fname = fs.stem(outfile)
    fname = fs.stem(fname)
    fname = fs.name(fname)

    custom_target(
        'test - extract json data @0@'.format(fname),
        depends: cargo_build,
        output: 'libtest-@0@.json'.format(fname),
        capture: true,
        command: [
            cargo_build,
            'inspect',
            bpf_o,
        ],
        build_by_default: true,
    )
    shared_library('test-@0@'.format(fname),
        src,
        c_args : ['-Wno-unknown-pragmas', '-DUDEV_HID_BPF_TEST_SUITE'],
        include_directories: include_directories('../src/bpf/'),
        install: false,
    )
endforeach

env = environment()
env.set('LD_LIBRARY_PATH', meson.current_build_dir())
pymod.find_installation(modules: ['pytest'])
pytest_args = [
    '--verbose',
    '-rfES',
    '--log-level=DEBUG',
    '--log-file', meson.project_build_root() / 'meson-logs' / 'pytest.log',
    '--log-file-level=DEBUG',
]

# pytest xdist is nice because it significantly speeds up our
# test process, but it's not required
if pymod.find_installation('python3', modules: ['xdist'], required: false).found()
    pytest_args += ['-n', 'auto']
endif

test('pytest',
    pytest,
    args: pytest_args,
    env: env,
    workdir: meson.project_source_root()
)
