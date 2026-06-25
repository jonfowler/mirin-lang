module ram (
    input  logic clk,
    input  logic [1:0] waddr,
    input  logic [7:0] wdata,
    input  logic we,
    input  logic [1:0] raddr,
    output logic [7:0] result
);
    logic [7:0] mem [0:3];
    initial begin
        mem = '{8'h10, 8'h20, 8'h30, 8'h40};
    end
    logic [7:0] __block_1 [0:3];
    always_comb begin
        __block_1 = mem;
        __block_1[waddr] = wdata;
    end
    logic [7:0] __block_0 [0:3];
    always_comb begin
        if (we) begin
            __block_0 = __block_1;
        end else begin
            __block_0 = mem;
        end
    end
    always_ff @(posedge clk) begin
        mem <= __block_0;
    end
    assign result = mem[raddr];
endmodule
