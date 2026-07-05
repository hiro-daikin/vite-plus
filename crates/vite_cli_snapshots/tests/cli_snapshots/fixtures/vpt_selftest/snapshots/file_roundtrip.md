# file_roundtrip

vpt setup/assertion helpers behave identically across platforms.

## `vpt write-file notes/hello.txt 'hello from vpt'`

```
```

## `vpt print-file notes/hello.txt`

```
hello from vpt
```

## `vpt stat-file notes/hello.txt missing.txt`

```
notes/hello.txt: exists
missing.txt: missing
```

## `vpt list-dir notes`

```
hello.txt
```

## `vpt json-edit package.json scripts.build 'vp build'`

```
```

## `vpt print-file package.json`

```
{
  "name": "vpt-selftest",
  "private": true,
  "scripts": {
    "build": "vp build"
  }
}
```

## `vpt touch-file created-by-touch.txt`

touch-file creates missing files

```
```

## `vpt stat-file created-by-touch.txt`

```
created-by-touch.txt: exists
```

## `vpt chmod +x created-by-touch.txt`

symbolic +x is accepted (no-op on Windows)

```
```

## `vpt pipe-stdin -- vpt read-stdin`

empty pipe-stdin data means empty stdin, not a bare newline

```
```

## `vpt pipe-stdin hello -- vpt read-stdin`

```
hello
```

## `vpt exit 3`

Nonzero exit codes are recorded in the snapshot.

**Exit code:** 3

```
```
