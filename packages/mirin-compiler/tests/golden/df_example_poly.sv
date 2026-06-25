module reg_fwd_uint (
    input  logic clk,
    input  logic inp__valid,
    input  logic [7:0] inp__data,
    output logic inp__ready,
    input  logic rst,
    output logic result__valid,
    output logic [7:0] result__data,
    input  logic result__ready
);
    DF__reg_fwd__uint8 DF__reg_fwd__uint8 (
        .clk(clk),
        .self__valid(inp__valid),
        .self__data(inp__data),
        .self__ready(inp__ready),
        .rst(rst),
        .result__valid(result__valid),
        .result__data(result__data),
        .result__ready(result__ready)
    );
endmodule

module df_sink_uint (
    input  logic clk,
    input  logic inp__valid,
    input  logic [7:0] inp__data,
    output logic inp__ready
);
    DF__df_sink__uint8 DF__df_sink__uint8 (
        .clk(clk),
        .self__valid(inp__valid),
        .self__data(inp__data),
        .self__ready(inp__ready)
    );
endmodule

module reg_fwd_uint_2 (
    input  logic clk,
    input  logic inp__valid,
    input  logic [7:0] inp__data,
    output logic inp__ready,
    input  logic rst,
    output logic result__valid,
    output logic [7:0] result__data,
    input  logic result__ready
);
    DF__reg_fwd_2__uint8 DF__reg_fwd_2__uint8 (
        .clk(clk),
        .self__valid(inp__valid),
        .self__data(inp__data),
        .self__ready(inp__ready),
        .rst(rst),
        .result__valid(result__valid),
        .result__data(result__data),
        .result__ready(result__ready)
    );
endmodule

module df_sink_uint_2 (
    input  logic clk,
    input  logic inp__valid,
    input  logic [7:0] inp__data,
    output logic inp__ready
);
    DF__df_sink_2__uint8 DF__df_sink_2__uint8 (
        .self__valid(inp__valid),
        .self__data(inp__data),
        .self__ready(inp__ready)
    );
endmodule

module DF__df_sink_2__uint8 (
    input  logic self__valid,
    input  logic [7:0] self__data,
    output logic self__ready
);
    assign self__ready = 1'b1;
endmodule

module DF__df_sink__uint8 (
    input  logic clk,
    input  logic self__valid,
    input  logic [7:0] self__data,
    output logic self__ready
);
    assign self__ready = 1'b1;
endmodule

module DF__reg_fwd_2__uint8 (
    input  logic clk,
    input  logic self__valid,
    input  logic [7:0] self__data,
    output logic self__ready,
    input  logic rst,
    output logic result__valid,
    output logic [7:0] result__data,
    input  logic result__ready
);
    logic en;
    logic reg_vld;
    logic [7:0] reg_data;
    logic out_rdy;
    assign en = ((!reg_vld) || out_rdy);
    logic __block_1;
    always_comb begin
        if (en) begin
            __block_1 = self__valid;
        end else begin
            __block_1 = reg_vld;
        end
    end
    logic __block_0;
    always_comb begin
        if (rst) begin
            __block_0 = 1'b0;
        end else begin
            __block_0 = __block_1;
        end
    end
    always_ff @(posedge clk) begin
        reg_vld <= __block_0;
    end
    logic [7:0] __block_2;
    always_comb begin
        if (en) begin
            __block_2 = self__data;
        end else begin
            __block_2 = reg_data;
        end
    end
    always_ff @(posedge clk) begin
        reg_data <= __block_2;
    end
    assign self__ready = en;
    assign out_rdy = result__ready;
    assign result__valid = reg_vld;
    assign result__data = reg_data;
endmodule

module DF__reg_fwd__uint8 (
    input  logic clk,
    input  logic self__valid,
    input  logic [7:0] self__data,
    output logic self__ready,
    input  logic rst,
    output logic result__valid,
    output logic [7:0] result__data,
    input  logic result__ready
);
    logic en;
    logic reg_vld;
    logic [7:0] reg_data;
    logic out_rdy;
    assign en = ((!reg_vld) || out_rdy);
    logic __block_1;
    always_comb begin
        if (en) begin
            __block_1 = self__valid;
        end else begin
            __block_1 = reg_vld;
        end
    end
    logic __block_0;
    always_comb begin
        if (rst) begin
            __block_0 = 1'b0;
        end else begin
            __block_0 = __block_1;
        end
    end
    always_ff @(posedge clk) begin
        reg_vld <= __block_0;
    end
    logic [7:0] __block_2;
    always_comb begin
        if (en) begin
            __block_2 = self__data;
        end else begin
            __block_2 = reg_data;
        end
    end
    always_ff @(posedge clk) begin
        reg_data <= __block_2;
    end
    assign self__ready = en;
    assign out_rdy = result__ready;
    assign result__valid = reg_vld;
    assign result__data = reg_data;
endmodule
