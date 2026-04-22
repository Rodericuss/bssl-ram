#!/usr/bin/env Rscript
# bench/analyze.R — parse all bench/results/*.txt, aggregate across runs,
# emit GitHub-flavoured-markdown tables on stdout, and render
# light/dark ggplot PNGs into bench/results/plots/.
#
# Each run of every bench script produces a timestamped *.txt under
# bench/results/. This script is idempotent: re-running it just folds
# whatever new files have shown up since the last invocation.
#
# Usage:
#   Rscript bench/analyze.R                # tables + plots, default paths
#   Rscript bench/analyze.R --no-plots     # markdown only
#
# Deps: ggplot2 + scales (install via `sudo Rscript -e
#       'install.packages(c("ggplot2","scales"))'`).

suppressPackageStartupMessages({
  library(ggplot2)
  library(scales)
})

all_args   <- commandArgs(trailingOnly = FALSE)
args       <- commandArgs(trailingOnly = TRUE)
make_plots <- !("--no-plots" %in% args)
# When --report is passed (default) the script also writes
# bench/REPORT.md next to the plots — the file the main README links
# to. Pass --no-report to suppress (useful for ad-hoc runs where you
# only want stdout).
make_report <- !("--no-report" %in% args)

# Locate ourselves: with `Rscript bench/analyze.R` the --file= arg
# carries the path; fall back to "." when sourced interactively.
script_arg <- grep("^--file=", all_args, value = TRUE)
script_dir <- if (length(script_arg)) {
  normalizePath(dirname(sub("^--file=", "", script_arg[1])))
} else {
  normalizePath(".")
}
repo_root <- normalizePath(file.path(script_dir, ".."))
results   <- file.path(repo_root, "bench", "results")
plots_dir <- file.path(results, "plots")
dir.create(plots_dir, recursive = TRUE, showWarnings = FALSE)

# ----------------------------------------------------------------------
# Visual identity. Keeps the repo's "Firefox-orange + cool charcoal"
# palette consistent across both ggplot output and the mermaid xycharts
# embedded in the README.
# ----------------------------------------------------------------------
brand <- list(
  orange   = "#ff7139",
  blue     = "#4285f4",
  electron = "#47848f",
  warn     = "#e15759",
  ok       = "#3a8d3a",
  ink_dark = "#0d1117",
  ink_lite = "#f8fafc",
  grid     = "#334155"
)

theme_bssl <- function(dark = FALSE) {
  bg <- if (dark) brand$ink_dark else brand$ink_lite
  fg <- if (dark) brand$ink_lite else brand$ink_dark
  grid_col <- if (dark) brand$grid else "#cbd5e1"
  theme_minimal(base_family = "sans", base_size = 13) +
    theme(
      plot.background  = element_rect(fill = bg, color = NA),
      panel.background = element_rect(fill = bg, color = NA),
      panel.grid.major = element_line(color = grid_col, linewidth = 0.3),
      panel.grid.minor = element_blank(),
      axis.text   = element_text(color = fg),
      axis.title  = element_text(color = fg, face = "bold"),
      plot.title  = element_text(color = fg, face = "bold", size = 16),
      plot.subtitle = element_text(color = fg, size = 11),
      plot.caption  = element_text(color = grid_col, size = 9, hjust = 0),
      legend.background = element_rect(fill = bg, color = NA),
      legend.text  = element_text(color = fg),
      legend.title = element_text(color = fg, face = "bold")
    )
}

## Plot saver expects a *builder* function so the inner geoms (notably
## the data labels) can pick a contrasting text colour per theme — a
## white label is invisible on the light background and vice versa.
save_pair <- function(builder, basename, w = 7, h = 4.2) {
  if (!make_plots) return(invisible(NULL))
  ggsave(file.path(plots_dir, paste0(basename, "-light.png")),
         builder(dark = FALSE) + theme_bssl(dark = FALSE),
         width = w, height = h, dpi = 160)
  ggsave(file.path(plots_dir, paste0(basename, "-dark.png")),
         builder(dark = TRUE) + theme_bssl(dark = TRUE),
         width = w, height = h, dpi = 160)
}

## Pick the in-bar / above-bar label colour given the theme.
label_color <- function(dark) {
  if (dark) brand$ink_lite else brand$ink_dark
}

# ----------------------------------------------------------------------
# Result-file parsers. Each returns a long-format data.frame ready for
# row-binding across timestamps when multiple runs exist.
# ----------------------------------------------------------------------

parse_cpu <- function(path) {
  txt <- readLines(path, warn = FALSE)
  # Lines look like:
  #   A1-procwalk       | ticks=28   | cpu_ms=280   | cpu%=0.0933
  rows <- grep("^A[0-9]+-", txt, value = TRUE)
  if (length(rows) == 0) return(NULL)
  do.call(rbind, lapply(rows, function(r) {
    label <- trimws(sub("\\|.*", "", r))
    ticks <- as.integer(sub(".*ticks=([0-9]+).*", "\\1", r))
    ms    <- as.numeric(sub(".*cpu_ms=([0-9.]+).*", "\\1", r))
    pct   <- as.numeric(sub(".*cpu%=([0-9.]+).*", "\\1", r))
    data.frame(
      run = sub(".*cpu-([0-9-]+)\\.txt$", "\\1", basename(path)),
      label = label, ticks = ticks, cpu_ms = ms, cpu_pct = pct,
      stringsAsFactors = FALSE
    )
  }))
}

parse_psi <- function(path) {
  txt <- readLines(path, warn = FALSE)
  rows <- grep("^B-", txt, value = TRUE)
  if (length(rows) == 0) return(NULL)
  do.call(rbind, lapply(rows, function(r) {
    label <- trimws(sub("\\|.*", "", r))
    if (grepl("NO COMPRESS", r)) {
      ms <- NA_real_
    } else {
      ms <- as.numeric(sub(".*reaction_ms=([0-9.]+).*", "\\1", r))
    }
    data.frame(
      run = sub(".*psi-latency-([0-9-]+)\\.txt$", "\\1", basename(path)),
      label = label, reaction_ms = ms,
      stringsAsFactors = FALSE
    )
  }))
}

parse_compress <- function(path) {
  txt <- readLines(path, warn = FALSE)
  before <- grep("^BEFORE", txt, value = TRUE)
  after  <- grep("^AFTER",  txt, value = TRUE)
  delta  <- grep("^RSS:",   txt, value = TRUE)
  if (length(before) == 0 || length(after) == 0) return(NULL)
  num <- function(line, key) {
    as.numeric(sub(paste0(".*", key, "=\\s*([0-9]+).*"), "\\1", line))
  }
  data.frame(
    run = sub(".*compress-real-([0-9-]+)\\.txt$", "\\1", basename(path)),
    rss_before_kib  = num(before[1], "RSS"),
    rss_after_kib   = num(after[1],  "RSS"),
    pss_before_kib  = num(before[1], "PSS"),
    pss_after_kib   = num(after[1],  "PSS"),
    swap_before_kib = num(before[1], "Swap"),
    swap_after_kib  = num(after[1],  "Swap"),
    syscall_ms      = if (length(grep("syscalls completed", txt))) {
      as.numeric(sub(".*in\\s+([0-9.]+)ms.*", "\\1",
                     grep("syscalls completed", txt, value = TRUE)[1]))
    } else NA_real_,
    stringsAsFactors = FALSE
  )
}

parse_recompress <- function(path) {
  txt <- readLines(path, warn = FALSE)
  total  <- as.integer(sub(".*: ([0-9]+)$", "\\1",
                           grep("^Total compress events", txt, value = TRUE)[1]))
  unique <- as.integer(sub(".*: ([0-9]+)$", "\\1",
                           grep("^Unique PIDs", txt, value = TRUE)[1]))
  recompress_line <- grep("^Recompressions",  txt, value = TRUE)[1]
  recompress <- as.integer(sub(".*: ([0-9]+)\\s+\\(.*", "\\1", recompress_line))
  pct        <- as.numeric(sub(".*\\(([0-9.]+)%.*", "\\1", recompress_line))
  data.frame(
    run = sub(".*recompress-([0-9-]+)\\.txt$", "\\1", basename(path)),
    total = total, unique_pids = unique, recompressions = recompress,
    rate_pct = pct,
    stringsAsFactors = FALSE
  )
}

# ----------------------------------------------------------------------
# Load + aggregate.
# ----------------------------------------------------------------------

load_all <- function(pattern, parser) {
  files <- list.files(results, pattern = pattern, full.names = TRUE)
  if (length(files) == 0) return(NULL)
  do.call(rbind, lapply(files, parser))
}

cpu        <- load_all("^cpu-.*\\.txt$",            parse_cpu)
psi        <- load_all("^psi-latency-.*\\.txt$",    parse_psi)
compress   <- load_all("^compress-real-.*\\.txt$",  parse_compress)
recompress <- load_all("^recompress-.*\\.txt$",     parse_recompress)

# ----------------------------------------------------------------------
# Markdown tables on stdout.
# ----------------------------------------------------------------------

## The markdown report is streamed to stdout *and* to
## bench/REPORT.md simultaneously, so `Rscript bench/analyze.R` both
## prints a summary and refreshes the file the project README links
## to. Everything below that uses cat() goes through tee_md().
report_path <- file.path(repo_root, "bench", "REPORT.md")
report_con  <- if (make_report) file(report_path, open = "w") else NULL
tee_md <- function(...) {
  msg <- paste0(..., collapse = "")
  cat(msg)
  if (!is.null(report_con)) cat(msg, file = report_con)
}

tee_md("# bssl-ram — benchmark report\n")
tee_md("\nGenerated: ", format(Sys.time(), "%Y-%m-%d %H:%M:%S %Z"), "\n", sep = "")
tee_md("\n> Regenerate with `Rscript bench/analyze.R`. Aggregated across every\n")
tee_md("> timestamped run currently under `bench/results/`.\n\n")

agg_summary <- function(df, group_col, value_col, fmt = "%.3f", unit = "") {
  g <- split(df[[value_col]], df[[group_col]])
  do.call(rbind, lapply(names(g), function(k) {
    v <- g[[k]][!is.na(g[[k]])]
    data.frame(
      label = k,
      n     = length(v),
      mean  = if (length(v)) sprintf(fmt, mean(v)) else "—",
      sd    = if (length(v) > 1) sprintf(fmt, sd(v)) else "—",
      min   = if (length(v)) sprintf(fmt, min(v)) else "—",
      max   = if (length(v)) sprintf(fmt, max(v)) else "—",
      stringsAsFactors = FALSE
    )
  }))
}

## Render a data.frame as a GitHub markdown table. Uses a for loop
## (not apply) so the side-effect writes inside tee_md hit BOTH the
## stdout cat() and the report file connection.
print_md_table <- function(df, headers, aligns = NULL) {
  if (is.null(aligns)) aligns <- rep(":---", ncol(df))
  tee_md("| ", paste(headers, collapse = " | "), " |\n")
  tee_md("| ", paste(aligns,  collapse = " | "), " |\n")
  for (i in seq_len(nrow(df))) {
    row <- as.character(unlist(df[i, ]))
    tee_md("| ", paste(row, collapse = " | "), " |\n")
  }
  tee_md("\n")
}

## Helper: embed a <picture> with both light + dark variants of a
## plot. Paths are relative to bench/REPORT.md so they resolve when the
## file is rendered on github.com.
embed_picture <- function(basename, alt) {
  tee_md('<picture>\n')
  tee_md('  <source media="(prefers-color-scheme: dark)" srcset="results/plots/',
         basename, '-dark.png">\n', sep = "")
  tee_md('  <img alt="', alt, '" src="results/plots/', basename, '-light.png">\n',
         sep = "")
  tee_md('</picture>\n\n')
}

# A — CPU
if (!is.null(cpu)) {
  tee_md("## Test A — daemon CPU per discovery mode\n\n")
  tee_md("Aggregated across ", length(unique(cpu$run)), " run(s).\n\n", sep = "")
  embed_picture("test-a-cpu", "Bar chart: daemon CPU % per discovery mode")
  agg <- agg_summary(cpu, "label", "cpu_pct", "%.4f", " %")
  print_md_table(agg,
    headers = c("Config", "Runs", "Mean CPU %", "Std-dev", "Min %", "Max %"),
    aligns  = c(":---", "---:", "---:", "---:", "---:", "---:"))
}

# B — PSI latency
if (!is.null(psi)) {
  tee_md("## Test B — PSI reaction latency under 14 GiB allocation\n\n")
  embed_picture("test-b-psi-latency",
                "Bar chart: PSI on vs timer-only reaction time")
  agg <- agg_summary(psi, "label", "reaction_ms", "%.0f", " ms")
  print_md_table(agg,
    headers = c("Mode", "Runs", "Mean ms", "Std-dev", "Min ms", "Max ms"),
    aligns  = c(":---", "---:", "---:", "---:", "---:", "---:"))
}

# C — real compression
if (!is.null(compress)) {
  tee_md("## Test C — real compression on largest renderer\n\n")
  embed_picture("test-c-rss-before-after",
                "RSS before/after compression of the largest renderer")
  cmp <- transform(compress,
    rss_drop_mib = (rss_before_kib - rss_after_kib) / 1024,
    rate_pct     = (rss_before_kib - rss_after_kib) / rss_before_kib * 100)
  rows <- data.frame(
    Run            = cmp$run,
    `Before MiB`   = sprintf("%d", cmp$rss_before_kib %/% 1024),
    `After MiB`    = sprintf("%d", cmp$rss_after_kib  %/% 1024),
    `Δ MiB`        = sprintf("%.0f", cmp$rss_drop_mib),
    `Δ %`          = sprintf("%.1f%%", cmp$rate_pct),
    `Syscall ms`   = sprintf("%.0f", cmp$syscall_ms),
    stringsAsFactors = FALSE,
    check.names      = FALSE
  )
  print_md_table(rows,
    headers = c("Run", "Before MiB", "After MiB", "Δ MiB", "Δ %", "Syscall ms"),
    aligns  = c(":---", "---:", "---:", "---:", "---:", "---:"))
}

# E — recompression
if (!is.null(recompress)) {
  tee_md("## Test E — recompression cascade prevention\n\n")
  embed_picture("test-e-recompression",
                "Unique compressions vs recompressions over a 90s aggressive window")
  rows <- data.frame(
    Run             = recompress$run,
    `Total events`  = sprintf("%d", recompress$total),
    `Unique PIDs`   = sprintf("%d", recompress$unique_pids),
    `Recompressions`= sprintf("%d", recompress$recompressions),
    `Rate %`        = sprintf("%.1f%%", recompress$rate_pct),
    stringsAsFactors = FALSE,
    check.names      = FALSE
  )
  print_md_table(rows,
    headers = c("Run", "Total events", "Unique PIDs", "Recompressions", "Rate %"),
    aligns  = c(":---", "---:", "---:", "---:", "---:"))
}

# ----------------------------------------------------------------------
# Plots.
# ----------------------------------------------------------------------

if (make_plots) {
  tee_md("## Plots\n\nWritten to `bench/results/plots/`:\n\n")
  if (!is.null(cpu)) {
    save_pair(function(dark) {
      ggplot(cpu, aes(x = label, y = cpu_pct, fill = label)) +
        geom_col(width = 0.6, show.legend = FALSE) +
        scale_fill_manual(values = c(brand$warn, brand$blue, brand$ok)) +
        geom_text(aes(label = sprintf("%.3f%%", cpu_pct)),
                  vjust = -0.5, size = 4.4, fontface = "bold",
                  color = label_color(dark)) +
        scale_y_continuous(labels = function(x) sprintf("%.3f%%", x),
                           expand = expansion(mult = c(0, .18))) +
        labs(title = "Daemon CPU per discovery mode",
             subtitle = "Lower is better — 300 s sample window per config",
             x = NULL, y = "Average CPU %",
             caption = "bench/scripts/bench-cpu.sh")
    }, "test-a-cpu", w = 7, h = 4.2)
    tee_md("- `test-a-cpu-{light,dark}.png`\n")
  }
  if (!is.null(psi)) {
    save_pair(function(dark) {
      ggplot(psi, aes(x = label, y = reaction_ms, fill = label)) +
        geom_col(width = 0.6, show.legend = FALSE) +
        scale_fill_manual(values = c(brand$ok, brand$warn)) +
        geom_text(aes(label = sprintf("%.0f ms", reaction_ms)),
                  vjust = -0.5, size = 4.4, fontface = "bold",
                  color = label_color(dark)) +
        scale_y_continuous(expand = expansion(mult = c(0, .18))) +
        labs(title = "Reaction latency under 14 GiB memory pressure",
             subtitle = "Lower is better — wall-clock from alloc start to first compress",
             x = NULL, y = "milliseconds",
             caption = "bench/scripts/bench-psi-latency.sh")
    }, "test-b-psi-latency", w = 7, h = 4.2)
    tee_md("- `test-b-psi-latency-{light,dark}.png`\n")
  }
  if (!is.null(compress)) {
    long <- rbind(
      data.frame(run = compress$run, phase = "Before", mib = compress$rss_before_kib / 1024),
      data.frame(run = compress$run, phase = "After",  mib = compress$rss_after_kib  / 1024)
    )
    long$phase <- factor(long$phase, levels = c("Before", "After"))
    save_pair(function(dark) {
      ggplot(long, aes(x = phase, y = mib, fill = phase)) +
        geom_col(width = 0.6, show.legend = FALSE) +
        scale_fill_manual(values = c(brand$warn, brand$ok)) +
        geom_text(aes(label = sprintf("%.0f MiB", mib)),
                  vjust = -0.4, size = 4.4, fontface = "bold",
                  color = label_color(dark)) +
        scale_y_continuous(expand = expansion(mult = c(0, .18))) +
        labs(title = "Real compression — RSS before vs after",
             subtitle = "Single largest renderer, one process_madvise sweep",
             x = NULL, y = "MiB",
             caption = "bench/scripts/bench-real-compress.sh")
    }, "test-c-rss-before-after", w = 6, h = 4.2)
    tee_md("- `test-c-rss-before-after-{light,dark}.png`\n")
  }
  if (!is.null(recompress)) {
    long <- rbind(
      data.frame(label = "Unique PIDs",   value = recompress$unique_pids),
      data.frame(label = "Recompressions", value = recompress$recompressions)
    )
    save_pair(function(dark) {
      ggplot(long, aes(x = label, y = value, fill = label)) +
        geom_col(width = 0.6, show.legend = FALSE) +
        scale_fill_manual(values = c(brand$ok, brand$warn)) +
        geom_text(aes(label = value), vjust = -0.4, size = 4.4,
                  fontface = "bold", color = label_color(dark)) +
        scale_y_continuous(expand = expansion(mult = c(0, .25))) +
        labs(title = "Recompression cascade prevention",
             subtitle = "Aggressive 90s window — every cycle re-evaluates every target",
             x = NULL, y = "events",
             caption = "bench/scripts/bench-recompression.sh")
    }, "test-e-recompression", w = 6, h = 4.2)
    tee_md("- `test-e-recompression-{light,dark}.png`\n")
  }
  tee_md("\n")
}

# Close the REPORT.md handle and surface its path on stderr so the
# build / dev workflow can pipe it somewhere useful.
if (!is.null(report_con)) {
  close(report_con)
  message("Wrote ", report_path)
}
