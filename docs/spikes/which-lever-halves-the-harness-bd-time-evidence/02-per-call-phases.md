# 2 — Where a call's time goes, in-process and through the CLI

## Server mode, production store (3,083 issues), reads only

    probe -dir <production> -n 10 -reads-only -label spira -ids <ten real ids>

    phase  process_start_to_open_call   wall      0.0 ms  cpu     48.4 ms
    phase  open_store                   wall     32.6 ms  cpu      5.8 ms

    op                    n   cpu_p50_ms   cpu_p90_ms   cpu_min_ms  wall_p50_ms  wall_min_ms     rows
    show                 10        2.567        8.283        2.044       12.593       12.119        1
    list-open            10        2.693        5.605        2.233       72.869       44.649       67
    list-all             10       62.599       76.834       55.355      217.030      157.966     3083
    list-by-label        10        1.680        7.746        1.587       61.222       44.375       33
    ready                10        2.672       11.977        2.522       86.180       53.036       63
    count                10        0.410        0.505        0.333        1.602        1.514       67
    count-by-label       10        1.231        1.295        1.118       61.497       57.572       37
    query                10        1.117        1.135        0.980       67.179       41.746        7
    related              10        0.554        1.061        0.429        3.138        2.982        0

The same nine calls as CLI processes, same ids, same session
(`poc/cli-bench-prod.sh <production> 10 <ids>`):

    op                    n   cpu_p50_ms   cpu_p90_ms   cpu_min_ms  wall_p50_ms  wall_min_ms
    version              10           80           90           70          110          100
    show                 10          100          100           90          160          120
    list-open            10          110          110          100          190          150
    list-all             10          320          360          310          620          510
    list-by-label        10          110          110          100          230          150
    ready                10          110          110          100          200          160
    count                10           90          100           90          150          120
    count-by-label       10           90          100           90          200          150
    query                10           90          100           90          180          130

Read the two together:

| | CLI | in-process | ratio |
|---|---|---|---|
| `show`, CPU | 100 ms | 2.6 ms | 38x |
| `show`, wall | 160 ms | 12.6 ms | 13x |
| `count`, CPU | 90 ms | 0.4 ms | 220x |
| `list --limit 0` (3,083 rows), CPU | 320 ms | 62.6 ms | 5.1x |
| `list --limit 0` (3,083 rows), wall | 620 ms | 217 ms | 2.9x |

`bd --version` is 80 ms of the CLI's 90-110 ms on every small call. The store work in a
small server-mode call is single-digit milliseconds, and the process is the rest.

The ratio collapses toward 3x as the result grows, because rendering 3,083 rows is real work
either way. The harness's calls are overwhelmingly the small kind.

## Embedded mode, 500-issue fixture

    probe -dir <bench-500> -n 10

    phase  process_start_to_open_call   wall      0.0 ms  cpu     48.7 ms
    phase  open_store                   wall   1519.1 ms  cpu     74.5 ms

    op                    n   cpu_p50_ms   cpu_p90_ms   cpu_min_ms  wall_p50_ms  wall_min_ms     rows
    show                 10      234.340      265.382      225.134      453.148      300.901        1
    list-open            10      188.406      202.271      169.018      311.359      204.161      333
    list-all             10      217.481      240.099      203.634      460.219      326.764      500
    list-by-label        10      180.585      193.222      175.290      437.103      332.576       47
    ready                10      150.744      165.571      137.853      275.502      202.450      333
    count                10       41.605       60.515       36.249      102.671       57.517      333
    count-by-label       10      110.839      126.785       97.302      248.768      162.076       10
    query                10       73.678       79.554       65.214      136.215       60.605      167
    related              10       45.361       61.755       41.156      105.277       54.070        0
    note                 10      116.182      126.193      103.939      218.737      179.945        1
    update               10       54.739       62.453       47.150       82.390       57.617        1
    create               10      217.707      225.009      201.442      340.353      272.334        1

The same twelve as CLI processes, on an identical copy of the same fixture
(`poc/cli-bench.sh <bench-500> 10`):

    op                    n   cpu_p50_ms   cpu_p90_ms   cpu_min_ms  wall_p50_ms  wall_min_ms
    version              10          140          150           90          210          120
    show                 10          610          860          560         1920          850
    list-open            10          610          670          590         5850          930
    list-all             10          630          640          610         1060          900
    list-by-label        10          590          660          560          940          780
    ready                10          410          440          400          640          560
    count                10          270          280          260          450          360
    count-by-label       10          340          350          330          510          470
    query                10          300          310          290          480          410
    note                 10          520          650          510          780          720
    update               10          500          510          420          750          690
    create               10          820          940          780         1410         1130

CPU ratio, CLI over in-process: show 2.6x, list-open 3.2x, list-all 2.9x, ready 2.7x,
count 6.5x, query 4.1x, note 4.5x, update 9.1x, create 3.8x. Median about 3.8x.

**The number that decides the shape of the answer**: an in-process call against embedded
Dolt still costs 150-235 ms of CPU, against 1-3 ms for the same call in-process against a
dolt server. The process boundary is worth 3-4x in embedded mode and 40-200x in server mode,
because in embedded mode the query itself is expensive and in server mode it is not. Holding
the store open removes the open; it does not make embedded Dolt fast.

An earlier 20-iteration run of the same embedded table, kept because it is the counter-example
to trusting wall time on this box:

    op                    n     p50_ms     p90_ms     min_ms     (wall)
    list-open            20   1358.849  61817.472    284.675

The 61.8-second p90 is the box. The CPU figures across the two runs agree to within 10%.
