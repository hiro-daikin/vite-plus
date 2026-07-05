# build_vite_env

## `VITE_MY_VAR=1 vp run build`

```
$ vp build
vite <version> building client environment for production...
transforming...✓ 4 modules transformed.
rendering chunks...
computing gzip size...
dist/index.html                <size> │ gzip: <size>
dist/assets/index-<hash>.js  <size> │ gzip: <size>

✓ built in <duration>
```

## `VITE_MY_VAR=1 vp run build`

should hit cache

```
$ vp build ◉ cache hit, replaying
vite <version> building client environment for production...
transforming...✓ 4 modules transformed.
rendering chunks...
computing gzip size...
dist/index.html                <size> │ gzip: <size>
dist/assets/index-<hash>.js  <size> │ gzip: <size>

✓ built in <duration>

---
vp run: cache hit, <duration> saved.
```

## `VITE_MY_VAR=2 vp run build`

env changed, should miss cache

```
$ vp build ○ cache miss: env 'VITE_MY_VAR' changed, executing
vite <version> building client environment for production...
transforming...✓ 4 modules transformed.
rendering chunks...
computing gzip size...
dist/index.html                <size> │ gzip: <size>
dist/assets/index-<hash>.js  <size> │ gzip: <size>

✓ built in <duration>
```
