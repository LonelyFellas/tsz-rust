# 本地密码阻止名单

来源：[SecLists Xato 1,000,000 passwords](https://raw.githubusercontent.com/danielmiessler/SecLists/c205c36a445bff37f8e58a9ec829105cd4975c58/Passwords/Common-Credentials/xato-net-10-million-passwords-1000000.txt)，提交 `c205c36a445bff37f8e58a9ec829105cd4975c58`。
原文件 SHA-256：`424a3e03a17df0a2bc2b3ca749d81b04e79d59cb7aeec8876a5a3f308d0caf51`。
源列表包含 1000000 行；本文件只保留满足 15–128 Unicode 字符范围的 10908 个不同密码的 SHA-256（排序、逐行、UTF-8 原样散列）。其余长度已被策略拒绝，无需重复存储。

这是有限的常见/泄露密码样本，不能保证覆盖全部泄露密码。运行时精确匹配整个密码，不把子串当作泄露匹配，也不记录或外传用户密码。
更新时核实新的 SecLists 固定提交，按相同规则生成列表，更新来源提交与校验和，并运行密码策略测试。

SecLists 的 MIT 许可见本目录 LICENSE；哈希仅用于阻止名单，不用于用户密码存储。
