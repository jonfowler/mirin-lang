module ram_write (
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
    always_ff @(posedge clk) begin
        if (we) mem[waddr] <= wdata;
    end
    assign result = mem[raddr];
endmodule
