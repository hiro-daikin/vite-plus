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

## `vpt exit 3`

Nonzero exit codes are recorded in the snapshot.

**Exit code:** 3

```
```
