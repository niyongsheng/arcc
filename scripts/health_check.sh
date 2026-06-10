#!/bin/bash
# ============================================================
# ARCC 系统健康检查脚本
# 兼容: macOS (Linux 可选适配)
# 用法: ./health_check.sh [--json] [--short]
# ============================================================
set -euo pipefail

# --- 参数解析 --------------------------------------------------
OUTPUT_MODE="human"   # human | json
SHORT_MODE=false

for arg in "$@"; do
    case "$arg" in
        --json) OUTPUT_MODE="json" ;;
        --short) SHORT_MODE=true ;;
        --help|-h)
            echo "用法: $0 [--json] [--short]"
            echo "  --json   以 JSON 格式输出"
            echo "  --short  精简模式，跳过网络测速等耗时项"
            exit 0
            ;;
    esac
done

# --- 颜色定义 --------------------------------------------------
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# --- 工具函数 --------------------------------------------------
now_iso() { date -u +"%Y-%m-%dT%H:%M:%SZ"; }
now_ts()  { date +%s; }

# 判定健康状态: 0=OK 1=WARN 2=CRIT
health_color() {
    case "$1" in
        OK)   echo -e "${GREEN}●${NC} OK" ;;
        WARN) echo -e "${YELLOW}●${NC} WARN" ;;
        CRIT) echo -e "${RED}●${NC} CRIT" ;;
        *)    echo -e "${CYAN}●${NC} $1" ;;
    esac
}

divider() {
    printf '%*s\n' "60" '' | tr ' ' '─'
}

# --- 检查模块 --------------------------------------------------

# 1. 系统基本信息
check_system_info() {
    local hostname_os cpu_model physical_mem uptime_str kernel_ver

    hostname_os=$(scutil --get ComputerName 2>/dev/null || hostname)
    cpu_model=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo "Unknown")
    physical_mem=$(sysctl -n hw.memsize 2>/dev/null | awk '{printf "%.1f GB", $1/1024/1024/1024}')
    kernel_ver=$(uname -r)
    uptime_str=$(uptime | sed 's/.*up //' | sed 's/,.*//')

    if [[ "$OUTPUT_MODE" == "json" ]]; then
        cat <<EOF
    "system": {
      "hostname": "$hostname_os",
      "cpu_model": "$cpu_model",
      "physical_memory": "$physical_mem",
      "kernel": "$kernel_ver",
      "uptime": "$uptime_str"
    },
EOF
    else
        echo -e "${BOLD}═══ 系统基本信息 ═══${NC}"
        echo -e "  主机名:     ${CYAN}$hostname_os${NC}"
        echo -e "  CPU:        ${CYAN}$cpu_model${NC}"
        echo -e "  物理内存:   ${CYAN}$physical_mem${NC}"
        echo -e "  内核版本:   ${CYAN}$kernel_ver${NC}"
        echo -e "  运行时间:   ${CYAN}$uptime_str${NC}"
    fi
}

# 2. CPU 使用率
check_cpu() {
    local cpu_usage load_avg cores load_per_core status status_code

    # macOS 用 top 采样一次取 CPU idle
    cpu_usage=$(top -l 1 -n 0 2>/dev/null | grep "CPU usage" | awk '{print $3}' | tr -d '%')
    cpu_usage=${cpu_usage:-0}
    # cpu_usage 是 user%，需要算 100-idle
    local idle
    idle=$(top -l 1 -n 0 2>/dev/null | grep "CPU usage" | awk '{print $7}' | tr -d '%')
    idle=${idle:-100}
    cpu_usage=$(echo "100 - $idle" | bc -l 2>/dev/null || echo "0")
    cpu_usage=$(printf "%.1f" "$cpu_usage")

    load_avg=$(sysctl -n vm.loadavg 2>/dev/null | awk '{print $2, $3, $4}')
    cores=$(sysctl -n hw.ncpu 2>/dev/null)
    load_per_core=$(echo "scale=2; $(echo "$load_avg" | awk '{print $1}') / $cores" | bc -l 2>/dev/null || echo "0")

    # 判定
    if (( $(echo "$cpu_usage > 90" | bc -l) )); then
        status="CRIT"; status_code=2
    elif (( $(echo "$cpu_usage > 70" | bc -l) )); then
        status="WARN"; status_code=1
    else
        status="OK"; status_code=0
    fi

    if [[ "$OUTPUT_MODE" == "json" ]]; then
        cat <<EOF
    "cpu": {
      "usage_percent": $cpu_usage,
      "load_avg_1_5_15": "$load_avg",
      "cores": $cores,
      "load_per_core": $load_per_core,
      "status": "$status"
    },
EOF
    else
        echo -e "${BOLD}═══ CPU ═══${NC}"
        echo -e "  使用率:     ${cpu_usage}%  $(health_color "$status")"
        echo -e "  负载均值:   ${CYAN}$load_avg${NC}  (${cores} 核心)"
        echo -e "  单核负载:   ${CYAN}$load_per_core${NC}"
    fi
}

# 3. 内存
check_memory() {
    local page_size total_pages free_pages used_pages
    local total_mem free_mem used_mem used_percent status status_code
    local mem_pressure

    page_size=$(sysctl -n hw.pagesize 2>/dev/null)
    # vm_stat 输出解析
    local vm_stats
    vm_stats=$(vm_stat)
    free_pages=$(echo "$vm_stats" | awk '/Pages free/ {print $3}' | tr -d '.')
    active_pages=$(echo "$vm_stats" | awk '/Pages active/ {print $3}' | tr -d '.')
    inactive_pages=$(echo "$vm_stats" | awk '/Pages inactive/ {print $3}' | tr -d '.')
    speculative_pages=$(echo "$vm_stats" | awk '/Pages speculative/ {print $3}' | tr -d '.')
    wired_pages=$(echo "$vm_stats" | awk '/Pages wired down/ {print $4}' | tr -d '.')
    compressed_pages=$(echo "$vm_stats" | awk '/Pages stored in compressor/ {print $5}' | tr -d '.')

    total_pages=$(echo "$free_pages + $active_pages + $inactive_pages + $speculative_pages + $wired_pages" | bc)
    total_mem=$(echo "scale=2; $total_pages * $page_size / 1024 / 1024 / 1024" | bc)
    used_mem=$(echo "scale=2; ($active_pages + $wired_pages + $compressed_pages) * $page_size / 1024 / 1024 / 1024" | bc)
    free_mem=$(echo "scale=2; $total_mem - $used_mem" | bc)
    used_percent=$(echo "scale=1; ($used_mem / $total_mem) * 100" | bc)

    # 内存压力 (macOS 专有)
    mem_pressure=$(sysctl -n kern.memorystatus_vm_pressure_level 2>/dev/null || echo "unknown")
    case "$mem_pressure" in
        1) mem_pressure="normal" ;;
        2) mem_pressure="warn" ;;
        4) mem_pressure="critical" ;;
    esac

    if (( $(echo "$used_percent > 95" | bc -l) )); then
        status="CRIT"; status_code=2
    elif (( $(echo "$used_percent > 80" | bc -l) )); then
        status="WARN"; status_code=1
    else
        status="OK"; status_code=0
    fi

    if [[ "$OUTPUT_MODE" == "json" ]]; then
        cat <<EOF
    "memory": {
      "total_gb": $total_mem,
      "used_gb": $used_mem,
      "free_gb": $free_mem,
      "used_percent": $used_percent,
      "pressure": "$mem_pressure",
      "status": "$status"
    },
EOF
    else
        echo -e "${BOLD}═══ 内存 ═══${NC}"
        echo -e "  总量:       ${CYAN}${total_mem} GB${NC}"
        echo -e "  已用:       ${CYAN}${used_mem} GB${NC} (${used_percent}%)  $(health_color "$status")"
        echo -e "  可用:       ${CYAN}${free_mem} GB${NC}"
        echo -e "  内存压力:   ${CYAN}$mem_pressure${NC}"
    fi
}

# 4. 磁盘
check_disk() {
    local root_usage root_total root_used root_free root_percent status status_code

    # macOS: 检查 / 和 /System/Volumes/Data
    local df_out
    df_out=$(df -h / 2>/dev/null | tail -1)
    root_usage=$(echo "$df_out" | awk '{print $5}' | tr -d '%')
    root_total=$(echo "$df_out" | awk '{print $2}')
    root_used=$(echo "$df_out" | awk '{print $3}')
    root_free=$(echo "$df_out" | awk '{print $4}')

    if (( root_usage > 95 )); then
        status="CRIT"; status_code=2
    elif (( root_usage > 80 )); then
        status="WARN"; status_code=1
    else
        status="OK"; status_code=0
    fi

    if [[ "$OUTPUT_MODE" == "json" ]]; then
        cat <<EOF
    "disk": {
      "mount": "/",
      "total": "$root_total",
      "used": "$root_used",
      "free": "$root_free",
      "used_percent": $root_usage,
      "status": "$status"
    },
EOF
    else
        echo -e "${BOLD}═══ 磁盘 ═══${NC}"
        echo -e "  挂载点 /:   ${CYAN}${root_usage}%${NC} 已用  $(health_color "$status")"
        echo -e "              总计 ${root_total} / 已用 ${root_used} / 可用 ${root_free}"
    fi

    # 如果磁盘紧张，列出前 5 大目录
    if (( root_usage > 80 )) && [[ "$SHORT_MODE" != true ]]; then
        echo -e "  ${YELLOW}⚠ 磁盘使用率高于 80%，扫描大目录…${NC}"
        du -sh /Users/nigang/* 2>/dev/null | sort -rh | head -5 | while read -r line; do
            echo -e "    $line"
        done
    fi
}

# 5. 网络连通性
check_network() {
    local dns_ok ping_ok external_ip status status_code

    # DNS 解析
    if dscacheutil -q host -a name google.com 2>/dev/null | grep -q "ip_address" || \
       nslookup google.com 2>/dev/null | grep -q "Address"; then
        dns_ok=true
    else
        dns_ok=false
    fi

    # 外网连通 (快速 ping)
    if ping -c 1 -W 2000 8.8.8.8 &>/dev/null; then
        ping_ok=true
    else
        ping_ok=false
    fi

    # 公网 IP (仅在非精简模式)
    if [[ "$SHORT_MODE" != true ]] && $ping_ok; then
        external_ip=$(curl -s --connect-timeout 3 https://ifconfig.me 2>/dev/null || echo "N/A")
    else
        external_ip="skipped"
    fi

    # 综合判定
    if $dns_ok && $ping_ok; then
        status="OK"; status_code=0
    elif $dns_ok || $ping_ok; then
        status="WARN"; status_code=1
    else
        status="CRIT"; status_code=2
    fi

    if [[ "$OUTPUT_MODE" == "json" ]]; then
        cat <<EOF
    "network": {
      "dns_resolution": $dns_ok,
      "internet_reachable": $ping_ok,
      "external_ip": "$external_ip",
      "status": "$status"
    },
EOF
    else
        echo -e "${BOLD}═══ 网络 ═══${NC}"
        echo -e "  DNS 解析:   $($dns_ok && echo -e "${GREEN}✓${NC}" || echo -e "${RED}✗${NC}")"
        echo -e "  外网连通:   $($ping_ok && echo -e "${GREEN}✓${NC}" || echo -e "${RED}✗${NC}")"
        echo -e "  公网 IP:    ${CYAN}$external_ip${NC}"
        echo -e "  综合状态:   $(health_color "$status")"
    fi
}

# 6. 进程 Top 5 (CPU / 内存)
check_processes() {
    if [[ "$OUTPUT_MODE" == "json" ]]; then
        echo '    "top_cpu": ['
        ps aux --sort=-%cpu 2>/dev/null | head -6 | tail -5 | awk '{printf "      {\"pid\": %s, \"name\": \"%s\", \"cpu\": %s, \"mem\": %s},\n", $2, $11, $3, $4}'
        echo '    ],'
        echo '    "top_mem": ['
        ps aux --sort=-%mem 2>/dev/null | head -6 | tail -5 | awk '{printf "      {\"pid\": %s, \"name\": \"%s\", \"cpu\": %s, \"mem\": %s},\n", $2, $11, $3, $4}'
        echo '    ],'
    else
        echo -e "${BOLD}═══ 进程 Top 5 (CPU) ═══${NC}"
        printf "  %-6s %-8s %-6s %s\n" "PID" "CPU%" "MEM%" "COMMAND"
        ps aux --sort=-%cpu 2>/dev/null | head -6 | tail -5 | awk '{printf "  %-6s %-8s %-6s %s\n", $2, $3, $4, $11}'
        echo ""
        echo -e "${BOLD}═══ 进程 Top 5 (内存) ═══${NC}"
        printf "  %-6s %-8s %-6s %s\n" "PID" "CPU%" "MEM%" "COMMAND"
        ps aux --sort=-%mem 2>/dev/null | head -6 | tail -5 | awk '{printf "  %-6s %-8s %-6s %s\n", $2, $3, $4, $11}'
    fi
}

# 7. 电池 (仅笔记本)
check_battery() {
    local battery_info cycle_count max_capacity health status_code

    if ! system_profiler SPPowerDataType &>/dev/null; then
        # 非笔记本或无法读取
        if [[ "$OUTPUT_MODE" == "json" ]]; then
            echo '    "battery": null,'
        fi
        return
    fi

    battery_info=$(pmset -g batt 2>/dev/null | tail -1)
    cycle_count=$(system_profiler SPPowerDataType 2>/dev/null | awk '/Cycle Count/ {print $3}')
    max_capacity=$(system_profiler SPPowerDataType 2>/dev/null | awk '/Maximum Capacity/ {print $3}' | tr -d '%')

    local charging_status percent
    percent=$(echo "$battery_info" | grep -oE '[0-9]+%' | tr -d '%')
    if echo "$battery_info" | grep -q "charging"; then
        charging_status="charging"
    elif echo "$battery_info" | grep -q "AC"; then
        charging_status="AC Power"
    elif echo "$battery_info" | grep -q "discharging"; then
        charging_status="discharging"
    else
        charging_status="unknown"
    fi

    # 判定
    if [[ "$charging_status" == "discharging" ]] && (( percent < 10 )); then
        status="CRIT"; status_code=2
    elif [[ "$charging_status" == "discharging" ]] && (( percent < 20 )); then
        status="WARN"; status_code=1
    else
        status="OK"; status_code=0
    fi

    if [[ "$OUTPUT_MODE" == "json" ]]; then
        cat <<EOF
    "battery": {
      "percent": $percent,
      "status": "$charging_status",
      "cycle_count": $cycle_count,
      "max_capacity_percent": $max_capacity,
      "health": "$status"
    },
EOF
    else
        echo -e "${BOLD}═══ 电池 ═══${NC}"
        echo -e "  电量:       ${percent}%  $(health_color "$status")"
        echo -e "  状态:       ${CYAN}$charging_status${NC}"
        echo -e "  循环次数:   ${CYAN}$cycle_count${NC}"
        echo -e "  最大容量:   ${CYAN}${max_capacity}%${NC}"
    fi
}

# 8. 安全审计 — 关键服务状态
check_security() {
    local sip_status fv_status ssh_status

    # SIP (System Integrity Protection)
    if csrutil status 2>/dev/null | grep -q "enabled"; then
        sip_status="enabled"
    else
        sip_status="disabled"
    fi

    # FileVault
    if fdesetup status 2>/dev/null | grep -q "On"; then
        fv_status="on"
    else
        fv_status="off"
    fi

    # SSH 远程登录
    if systemsetup -getremotelogin 2>/dev/null | grep -q "On"; then
        ssh_status="on"
    else
        ssh_status="off"
    fi

    if [[ "$OUTPUT_MODE" == "json" ]]; then
        cat <<EOF
    "security": {
      "sip": "$sip_status",
      "filevault": "$fv_status",
      "remote_login_ssh": "$ssh_status"
    }
EOF
    else
        echo -e "${BOLD}═══ 安全状态 ═══${NC}"
        [[ "$sip_status" == "enabled" ]] && local sip_color="$GREEN" || sip_color="$RED"
        [[ "$fv_status" == "on" ]] && local fv_color="$GREEN" || fv_color="$YELLOW"
        [[ "$ssh_status" == "off" ]] && local ssh_color="$GREEN" || ssh_color="$YELLOW"
        echo -e "  SIP:        ${sip_color}${sip_status}${NC}"
        echo -e "  FileVault:  ${fv_color}${fv_status}${NC}"
        echo -e "  SSH 远程:   ${ssh_color}${ssh_status}${NC}"
    fi
}

# --- 主入口 --------------------------------------------------
main() {
    if [[ "$OUTPUT_MODE" == "json" ]]; then
        echo "{"
        echo "  \"timestamp\": \"$(now_iso)\","
        check_system_info
        check_cpu
        check_memory
        check_disk
        check_network
        check_processes
        check_battery
        check_security
        echo "  \"overall_status\": \"OK\""
        echo "}"
    else
        # 终端美观输出
        echo ""
        echo -e "${BOLD}╔══════════════════════════════════════════════════════╗${NC}"
        echo -e "${BOLD}║       ARCC 系统健康检查  |  $(date '+%Y-%m-%d %H:%M:%S')         ║${NC}"
        echo -e "${BOLD}╚══════════════════════════════════════════════════════╝${NC}"
        echo ""
        check_system_info
        echo ""; divider; echo ""
        check_cpu
        echo ""; divider; echo ""
        check_memory
        echo ""; divider; echo ""
        check_disk
        echo ""; divider; echo ""
        check_network
        echo ""; divider; echo ""
        check_processes
        echo ""; divider; echo ""
        check_battery
        echo ""; divider; echo ""
        check_security
        echo ""
        divider
        echo -e "${BOLD}检查完成 @ $(date '+%H:%M:%S')${NC}"
        echo ""
    fi
}

main
