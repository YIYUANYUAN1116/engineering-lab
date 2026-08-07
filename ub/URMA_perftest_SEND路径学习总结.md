# URMA perftest SEND 数据路径学习总结

## 1. 研究目标

本文记录对 UMDK/URMA perftest 源码阅读过程中的核心理解，重点关注：

- URMA 通信资源建立流程
- SEND/RECV 数据路径
- WR、SGE、TSEG、JFC、CQE 等核心概念
- 与后续 Dragonfly 接入 UB/URMA 的对应关系

当前结论主要来自源码阅读和架构分析。

------------------------------------------------------------------------

# 2. URMA 整体通信模型

URMA 与 TCP 最大区别：

TCP：

    socket
      |
    write()
      |
    kernel buffer
      |
    network

应用只需要发送数据，内核负责连接、缓存和完成通知。

URMA：

    buffer
      |
    register memory
      |
    create WR
      |
    post WR
      |
    hardware execute
      |
    completion event

应用需要显式管理：

- 通信端点
- 内存注册
- 发送/接收队列
- 完成通知

------------------------------------------------------------------------

# 3. URMA 初始化流程

整体流程：

    urma_init()

        |
        v

    urma_create_context()

        |
        v

    create JFC

        |
        v

    create Jetty

        |
        v

    register memory segment

        |
        v

    exchange connection info

        |
        v

    import remote Jetty

        |
        v

    bind Jetty

        |
        v

    ready for data transfer

------------------------------------------------------------------------

# 4. 核心对象理解

## 4.1 Context

表示 URMA 设备上下文。

类似：

- 初始化设备
- 创建通信资源的入口

------------------------------------------------------------------------

## 4.2 Jetty

可以理解为 URMA 通信端点。

类似 TCP socket，但不是流连接。

包含：

- JFS（发送）
- JFR（接收）

------------------------------------------------------------------------

## 4.3 JFS

Jetty Function Send。

发送队列。

应用提交：

    JFS WR

然后硬件执行发送。

------------------------------------------------------------------------

## 4.4 JFR

Jetty Function Receive。

接收队列。

与 TCP 不同：

接收 buffer 需要提前准备。

流程：

    准备 receive buffer

            |

    post receive WR

            |

    等待远端 SEND

------------------------------------------------------------------------

## 4.5 JFC

Jetty Function Completion。

完成队列。

用于保存操作完成事件。

------------------------------------------------------------------------

## 4.6 CQE

Completion Queue Entry。

表示一次操作完成。

例如：

    post_send()

        |

    hardware

        |

    CQE

        |

    poll JFC

应用通过 CQE 知道：

- 哪个操作完成
- 状态是否成功

------------------------------------------------------------------------

# 5. cq_mod 机制

cq_mod：

completion moderation。

作用：

减少 completion 数量。

例如：

cq_mod = 4

发送：

    WR0
    WR1
    WR2
    WR3

只有：

    WR3

产生 CQE。

一个 CQE：

代表多个 WR 完成。

因此代码：

    ccnt += cfg->cq_mod

------------------------------------------------------------------------

# 6. 内存模型

## 6.1 local_buf

实际数据 buffer。

例如：

    0x100000

保存：

发送数据或者接收数据。

------------------------------------------------------------------------

## 6.2 local_tseg

来源：

    urma_register_seg()

作用：

注册内存。

包含：

- 地址范围
- 权限
- token/key

告诉硬件：

这块内存允许访问。

------------------------------------------------------------------------

## 6.3 local_sge

Scatter Gather Entry。

描述一次数据位置。

包含：

    addr
    len
    tseg

关系：

    local_sge

        addr
          |
          v

    实际buffer


        tseg

          |
          v

    注册信息

------------------------------------------------------------------------

# 7. WR（Work Request）

WR 是一次硬件操作描述。

不是数据本身。

类似：

    任务单

包含：

- opcode
- target jetty
- SGE
- completion配置
- user_ctx

------------------------------------------------------------------------

# 8. SEND 数据流程

## 8.1 prepare 阶段

prepare_jfs_wr():

创建：

    JFS WR

    +
    SGE

流程：

    buffer

      |

    local_sge

      |

    WR

------------------------------------------------------------------------

## 8.2 发送阶段

run_once_bw():

核心：

    urma_post_jfs_wr()

    或者

    urma_post_jetty_send_wr()

提交 WR。

流程：

    WR

     |

    JFS

     |

    hardware

     |

    remote JFR

------------------------------------------------------------------------

# 9. RECV 数据流程

接收端：

prepare_jfr_wr():

准备：

    receive buffer

        |

    local_sge

        |

    JFR WR

然后：

    urma_post_jfr_wr()

    或者

    urma_post_jetty_recv_wr()

提前告诉硬件：

数据来了放哪里。

------------------------------------------------------------------------

# 10. SEND/RECV 完整流程

    Sender                              Receiver


    buffer                              buffer

      |                                   |

    SGE                                 SGE

      |                                   |

    JFS WR                              JFR WR

      |                                   |

    post_send                           post_recv

              \                       /

                  UB network

              /                       \

    completion                         completion

------------------------------------------------------------------------

# 11. 与 Dragonfly 对应关系

Dragonfly 当前：

    TcpStream

    write(request)

    read(piece)

属于：

    stream 模型

URMA：

    construct WR

    post send

    wait completion

属于：

    message + completion 模型

------------------------------------------------------------------------

未来 Dragonfly 接 URMA：

可能流程：

    Peer connection建立

        |
        +-- 创建 Jetty

        |
        +-- 注册 Piece buffer

        |
        +-- 创建 JFS/JFR

        |
        +-- exchange endpoint

        |
        +-- bind

        |
        +-- download_piece()

                |
                +-- create WR

                |
                +-- post send

                |
                +-- wait completion

------------------------------------------------------------------------

# 12. 当前阶段结论

已经确认：

1.URMA 不是简单替换 TCP socket API。
2.URMA 是显式管理资源的数据通信模型。
3.Jetty/JFS/JFR 属于长期连接资源。
4.WR 是一次数据操作描述。
5.SGE 描述数据位置。
6.SEG 描述注册内存权限。
7.CQE 用于异步完成通知。
8.cq_mod 用于降低 completion 开销。

后续研究方向：

- Dragonfly Peer/TCPClient 与 URMA 模型映射
- Piece buffer 如何注册为 URMA Segment
- SEND/RECV 与 Piece 传输协议设计
- 是否需要 UBS Communication 更高层封装
