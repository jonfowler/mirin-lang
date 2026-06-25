module regbank #(parameter int N) (
    input  logic clk,
    input  logic rstn,
    input  logic [7:0] inp,
    output logic [7:0] result
);
    function automatic int sum_to(input int n);
        int acc;
        acc = 0;
        for (int i = 0; i < n; i++) begin
            acc = (acc + i);
        end
        return acc;
    endfunction
    localparam int d = sum_to(N);
    logic [7:0] regs [0:d-1];
    for (genvar i = 0; i < d; i++) begin : g_i
        logic [7:0] __block_0;
        always_ff @(posedge clk) begin
            if (!rstn) begin
                __block_0 <= 0;
            end else begin
                __block_0 <= inp;
            end
        end
        assign regs[i] = __block_0;
    end
    assign result = regs[0];
endmodule
