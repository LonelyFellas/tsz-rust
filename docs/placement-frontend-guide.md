# 定级测试 · 前端施工指南

> 配套文档:[产品方案](placement-product-plan.md) · 交互参照:[原型源码](prototype/placement-prototype.html)(浏览器直接打开可跑)
> 分工:你写实现,我 review 交互边界与状态逻辑。本文只给结构、契约和验收标准,不含实现代码。

---

## 1. 目录结构建议(Next.js App Router)

```
app/placement/page.tsx              # 入口,挂 PlacementFlow
components/placement/
  PlacementFlow.tsx                 # 屏幕状态机:welcome | quiz | result | invalid
  WelcomeScreen.tsx                 # 两态:新用户卖点 / 老用户词汇档案
  QuizScreen.tsx                    # 进度条 + 卡片堆 + 按钮
  SwipeCard.tsx                     # 滑卡手势(独立组件,只发 onAnswer 事件)
  ResultScreen.tsx / InvalidScreen.tsx
  ConfirmSheet.tsx                  # 底部确认抽屉(重测/退出共用)
lib/assessment/
  types.ts                          # API 契约类型(见 §2,唯一事实来源)
  client.ts                         # AssessmentClient 接口 + 按环境选择实现
  mock.ts                           # mock 实现(原型 mockApi 几乎原样移植)
  http.ts                           # 真实现,后端就绪后写,组件层零改动
```

关键约束:**组件只 import `client.ts` 的接口,永不直接碰 `mock.ts`**。真假词知识只存在于 mock 闭包 / 后端,组件拿到的类型里没有 `kind`、`band`。

## 2. API 契约类型(与后端方案 §8.2 一一对应)

```ts
// lib/assessment/types.ts
export type Band = 'A1' | 'A2' | 'B1' | 'B2' | 'C1' | 'C2';

export interface BlockItem { item_id: string; text: string }          // 永不含 kind/band
export interface Answer { item_id: string; known: boolean; rt_ms: number }

export interface StartResponse { session_id: string; block: BlockItem[] }

export type SubmitResponse =
  | { next_block: BlockItem[] }
  | { result: { state: 'completed'; band: Band; vocab_range: string } }
  | { result: { state: 'invalid'; reason: 'too_many_false_alarms' } };

export interface AssessmentClient {
  start(): Promise<StartResponse>;                       // 403 quota_exhausted → 抛 QuotaError
  submit(sessionId: string, answers: Answer[]): Promise<SubmitResponse>;
  resume(sessionId: string): Promise<StartResponse | SubmitResponse>;  // 断线恢复,mock 阶段可先 throw
}
```

## 3. Mock 移植要点

原型里的 `mockApi` 闭包(词库、buildBlock、升降档、FA 判定)可以近乎原样搬进 `mock.ts`,注意三点:

1. **加 100–300ms 的人工延迟**(`setTimeout` 包一层 Promise)——逼着你从第一天就做块提交的 loading 态,不要等接真后端才发现。
2. 3 次机会计数在 mock 里继续用 localStorage;真实现走后端 403。`client.ts` 把两者统一成 `QuotaError`。
3. 参数(块大小 5、升档线 3、FA 线 3/2、起测 A2…)抽成常量对象,和产品方案 §7.5 的参数表同名。

## 4. 移植时的交互规格(验收线)

从原型抄的时候,这些数值/行为是规格,不是随意值:

| 项 | 规格 |
|---|---|
| 滑动方向 | 左滑=认识,右滑=不认识(代码里单一常量,可切) |
| 触发阈值 | 90px,不足弹回(spring 曲线 ~320ms) |
| 方向印章 | 拖动中渐显,opacity = min(1, |dx|/90);松手前可见 |
| 飞出动画 | ~300ms,期间锁输入(flying 标志),防双答 |
| 卡片堆 | 背面 2 张(scale .945/.89),块边界无感 |
| 进度文案 | `NN / 约 25`,等宽数字;完成时跳满 |
| rt_ms | 卡片展示到作答的毫秒数,随块提交 |
| 减弱动态 | `prefers-reduced-motion` 下全部降级为瞬时切换 |
| 键盘 | ←=认识,→=不认识(桌面端) |

新增(原型没有、实现必须有):

- **块提交 loading**:第 5 张卡飞出后、下一块到达前,卡背呼吸动画;失败可重试,**已答的 5 题不丢**(答案在提交成功前留在内存)。
- **首题手势教学**:第一张卡轻晃或手指图标划过,只在首次测试出现。
- **session 恢复入口**:`session_id` 落 localStorage;进页面先查未完成 session。mock 阶段 `resume()` 可以不实现,但 `PlacementFlow` 里留好这个分支。

## 5. 样式 token(Tailwind v4 `@theme`)

原型 CSS 变量 → token 名建议,亮暗两套都在原型 `<style>` 头部,直接抄值:

```
--color-surface / --color-surface-2 / --color-ink / --color-muted / --color-faint
--color-line / --color-accent / --color-accent-deep / --color-accent-soft
--color-warn / --color-warn-soft
--font-serif(词卡)/ --font-sans(UI)/ --font-mono(数字)
```

暗色用 `@media (prefers-color-scheme)` + `data-theme` 双通道(原型的写法照搬)。

## 6. 施工顺序与验收

按依赖排序,每步可独立验收:

1. **types.ts + mock.ts + client.ts** —— 验收:node 里裸调 mock 走完一次全对/全错/乱点三种流程,返回符合契约。
2. **PlacementFlow 状态机 + 三屏静态版**(不含滑卡,先用按钮作答)—— 验收:按钮点完全流程,invalid / quota 分支可达。
3. **SwipeCard 手势** —— 验收:阈值弹回、印章渐显、飞出锁输入、reduced-motion 降级,四条全过。
4. **3 次机会 + 结果留存 + 确认抽屉** —— 验收:测 3 次后入口锁定;无效/退出/跳过不扣次数。
5. **loading/失败重试 + 首题教学** —— 验收:mock 延迟拉到 2s,块间体验不空白;断网重试不丢答案。
6. **样式对齐原型**(token、动效、暗色)—— 验收:与原型并排肉眼比对。
7. (后端就绪后)**http.ts + 切换开关** —— 验收:切换后 1–6 的行为零变化。

每完成一步把代码丢过来 review;第 3 步(手势)和第 4 步(次数规则)是逻辑密度最高的两处,建议单独发。
