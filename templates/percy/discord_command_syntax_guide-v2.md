# Command Syntax & Flags Guide

Welcome to the comprehensive command syntax guide. This document explains how to read and interpret command arguments, parameters, and POSIX-like flags when interacting with our Discord bot.

---

## 🛠️ Command Argument Overview

> ⚠️ **Important:** Type command arguments **without** the brackets (`< >` or `[ ]`) shown in e.g. the help menu!

### Argument Syntax Reference

| Syntax Pattern                                                     | Argument Type          | Description & Usage                                                                                                |
|:-------------------------------------------------------------------|:-----------------------|:-------------------------------------------------------------------------------------------------------------------|
| **`<argument>`**                                                   | Required               | This argument is **mandatory**. The command will fail or prompt you if this is omitted.                            |
| **`[argument]`**                                                   | Optional               | This argument is **optional**. Leaving it out will use a default behavior or skip the property.                    |
| **`<A\|B>`**                                                       | Multiple Choice        | You must choose exactly **one** of the provided options (either `A` or `B`).                                       |
| **`<argument...>`**                                                | Multi-Word / Greedy    | Consumes multiple arguments or the remainder of the message (e.g., a reason, description, or message content).     |
| **`<"argument">`**                                                 | Exact / Case-Sensitive | This argument is case-sensitive and should be typed exactly as shown in the help menu.                             |
| **`<argument="A">`**                                               | Default Value          | If you do not provide this argument, it automatically defaults to **"A"**.                                         |
| **`[--flag]`**<br>**`[--name <arg>]`**<br>**`[--name <arg="A">]`** | Command Flag           | This argument is a POSIX-like flag. It can be placed anywhere in the command string. See below for detailed rules. |

---

## 🚩 Command Flags Guide

Flags are advanced, POSIX-like arguments that can be passed to a command. They provide a clean way to toggle features, filter data, or pass optional key-value inputs. 

### Key Characteristics:
* **Order Independent:** Flags can be placed anywhere in the command parameters, in any order.
* **Prefixes:** Long flags are prefixed with `--`, while short-hand flags are prefixed with `-`.

### 1. Boolean (Store-True) Flags
Flags that take no value (shown as `[--flag1]`) represent a boolean (`True`/`False`) switch.
* If the flag is **not present**, its value defaults to `False`.
* If the flag is **present**, its value becomes `True`.

📝 **Example:**
```bash
!command --flag1 --flag2
```
*(Both `--flag1` and `--flag2` are evaluated as `True`)*

### 2. Valued Flags
A flag that takes a value (shown as `[--flag1 <argument>]`) is a standard valued option. If you include the flag, you **must** provide its corresponding value immediately following it.

📝 **Example:**
```bash
!command --flag1 this is flag1 text --flag2 value_for_flag2
```
* `--flag1` receives the value `"this is flag1 text"`
* `--flag2` receives the value `"value_for_flag2"`

### 3. Short-hand & Chained Flags
Short-hand flags use a single dash (`-`) and a single letter. You can chain multiple short-hand flags together **as long as the preceding flags do not require arguments**.

* **Standard combination:** `-a -b` can be compressed into `-ab`.
* **With trailing arguments:** The *last* flag in a short-hand combination can accept an argument.

📝 **Examples:**
```bash
!command -ab
```
*(Equivalent to running: `!command --a --b`)*

```bash
!command -abc "my value"
```
*(Equivalent to running: `!command --a --b --c "my value"`, where flags `a` and `b` are boolean switches, and flag `c` takes the value `"my value"`)*

---

## 💡 Practical Examples & Best Practices

Here is how to translate help definitions into actual typed commands:

### Case A: The Clean Ban Command
* **Definition:** `!ban <user> [reason...] [--silent]`
* **How to type it:** 
  ```bash
  !ban @Username Toxic behavior in chat --silent
  ```

### Case B: The Advanced Search Command
* **Definition:** `!search <query> [--limit <number=10>] [--sort <date|relevance>]`
* **How to type it (Default):**
  ```bash
  !search "discord python"
  ```
* **How to type it (Customized):**
  ```bash
  !search "discord python" --limit 5 --sort date
  ```
