package vixen

// HTTP date parsing (IMF-fixdate, RFC 850, asctime) -> unix seconds.
// Used for Date, Expires, Last-Modified, If-Modified-Since, cookie Expires.

import "core:strings"

days_from_civil :: proc(y, m, d: int) -> int {
	yy := y - (1 if m <= 2 else 0)
	era := (yy if yy >= 0 else yy - 399) / 400
	yoe := yy - era * 400
	doy := (153 * (m + (1 if m > 2 else -11)) + 2) / 5 + d - 1
	doe := yoe * 365 + yoe / 4 - yoe / 100 + doy
	return era * 146097 + doe - 719468
}

month_num :: proc(mon: string) -> int {
	switch mon {
	case "Jan":
		return 1
	case "Feb":
		return 2
	case "Mar":
		return 3
	case "Apr":
		return 4
	case "May":
		return 5
	case "Jun":
		return 6
	case "Jul":
		return 7
	case "Aug":
		return 8
	case "Sep":
		return 9
	case "Oct":
		return 10
	case "Nov":
		return 11
	case "Dec":
		return 12
	}
	return 0
}

atoi_n :: proc(s: string) -> (int, bool) {
	n := 0
	if len(s) == 0 {
		return 0, false
	}
	for c in s {
		if c < '0' || c > '9' {
			return 0, false
		}
		n = n * 10 + int(c - '0')
	}
	return n, true
}

// Cookie-date parsing is lenient (RFC 6265 §5.1.1); HTTP dates are strict.
// One lenient parser serves both: split on delimiters, find day/month/year/time.
http_date_parse :: proc(s: string) -> (int, bool) {
	toks: [dynamic]string
	defer delete(toks)
	cur: [dynamic]u8
	defer delete(cur)
	flush := false
	_ = flush
	for c in s {
		if (c >= '0' && c <= '9') || (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') {
			append(&cur, u8(c))
		} else if c == ':' {
			append(&cur, ':')
		} else {
			if len(cur) > 0 {
				append(&toks, string(cur[:]))
				cur = nil
			}
		}
	}
	if len(cur) > 0 {
		append(&toks, string(cur[:]))
	}
	day, year, month := -1, -1, 0
	hh, mm, ss := -1, -1, -1
	for t in toks {
		if strings.contains(t, ":") {
			parts := strings.split(t, ":", context.temp_allocator)
			if len(parts) == 3 {
				if h, ok := atoi_n(parts[0]); ok {
					hh = h
				}
				if m, ok := atoi_n(parts[1]); ok {
					mm = m
				}
				if s2, ok := atoi_n(parts[2]); ok {
					ss = s2
				}
			}
			continue
		}
		if m := month_num(t); m != 0 {
			month = m
			continue
		}
		if n, ok := atoi_n(t); ok {
			if len(t) >= 3 && len(t) == 4 {
				year = n
			} else if n > 31 {
				year = n
			} else if len(t) == 2 && year < 0 && day >= 0 {
				// Ambiguous 2-digit: year if a day was already seen
				// (RFC 850 puts year last), else day.
				year = n
			} else if day < 0 {
				day = n
			} else if year < 0 {
				year = n
			}
			continue
		}
		// Weekday names and GMT/UTC markers fall through here.
	}
	if year >= 0 && year < 100 {
		year += 2000 if year < 70 else 1900
	}
	if day < 0 || month == 0 || year < 0 || hh < 0 || mm < 0 || ss < 0 {
		return 0, false
	}
	if day > 31 || hh > 23 || mm > 59 || ss > 60 {
		return 0, false
	}
	return days_from_civil(year, month, day) * 86400 + hh * 3600 + mm * 60 + ss, true
}
