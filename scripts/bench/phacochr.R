#!/usr/bin/env Rscript
# PhacochR file adapter for `crabfinder-eval --benchmark --geocoder phacochr`
# (v3 Phase 11.1).
#
#   Rscript scripts/bench/phacochr.R input.csv output.csv
#
# input:  id,query,street,house_number,postal_code,city  (written by the adapter)
# output: id,lat,lon,type_geocoding                       (WGS84)
#
# Rows with street + number + postal code go through phacochr's structured
# columns; the others through its single "number, street, postal code"
# column, with the free-text query. Needs the phacochr package and its
# data (installed below when missing):
#   remotes::install_github("phacochr/phacochr"); phacochr::phaco_setup_data()

args <- commandArgs(trailingOnly = TRUE)
if (length(args) != 2) stop("usage: phacochr.R input.csv output.csv")

suppressPackageStartupMessages({
  library(phacochr)
  library(sf)
})

data_dir <- file.path(rappdirs::user_data_dir("phacochr"), "data_phacochr")
if (!dir.exists(data_dir) || length(list.files(data_dir)) == 0) {
  phaco_setup_data()
}

d <- read.csv(args[1], colClasses = "character", na.strings = character(0))
structured <- nzchar(d$street) & nzchar(d$house_number) & nzchar(d$postal_code)

geocode <- function(rows, ...) {
  if (nrow(rows) == 0) return(NULL)
  r <- phaco_geocode(rows, ...)$data_geocoded
  ok <- !is.na(r$x_31370) & !is.na(r$y_31370)
  out <- data.frame(id = r$id, lat = NA_real_, lon = NA_real_,
                    type_geocoding = ifelse(is.na(r$type_geocoding), "", r$type_geocoding))
  if (any(ok)) {
    pts <- st_as_sf(r[ok, c("x_31370", "y_31370")], coords = c("x_31370", "y_31370"), crs = 31370)
    ll <- st_coordinates(st_transform(pts, 4326))
    out$lon[ok] <- ll[, 1]
    out$lat[ok] <- ll[, 2]
  }
  out
}

a <- geocode(d[structured, c("id", "street", "house_number", "postal_code")],
             colonne_rue = "street", colonne_num = "house_number",
             colonne_code_postal = "postal_code")
free <- d[!structured, c("id", "query")]
names(free) <- c("id", "adresse")
b <- geocode(free, colonne_num_rue_code_postal = "adresse")

write.csv(rbind(a, b), args[2], row.names = FALSE, na = "")
